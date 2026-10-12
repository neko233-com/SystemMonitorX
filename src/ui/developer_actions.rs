use super::*;
use anyhow::{Context as _, ensure};
use system_monitor_x::network_ports::PortSnapshot;
use tables::{ProcessActionRequest, ProcessRowAction};

impl SystemMonitorApp {
    pub(super) fn copy_developer_text(
        &mut self,
        text: String,
        label: UiMessage,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.message = Some(mlf("已复制{label}", &[("label", label)]));
        cx.notify();
    }

    pub(super) fn handle_process_action(
        &mut self,
        request: ProcessActionRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = request.target;
        match request.action {
            ProcessRowAction::CopyPid => {
                self.copy_developer_text(target.pid.to_string(), " PID".to_owned().into(), cx)
            }
            ProcessRowAction::CopyPath => {
                if let Some(path) = target.exe {
                    self.copy_developer_text(
                        path.to_string_lossy().into_owned(),
                        msg("程序路径"),
                        cx,
                    );
                }
            }
            ProcessRowAction::QueryPorts => {
                self.set_page(Page::Ports, window, cx);
                self.search.update(cx, |input, cx| {
                    input.set_value(format!("pid:{}", target.pid), window, cx)
                });
                self.filter(cx);
            }
            ProcessRowAction::OpenFileLocation => {
                if let Some(path) = target.exe {
                    self.run_operation(
                        "打开文件位置",
                        move || {
                            platform::open_file_location(&path.to_string_lossy())?;
                            Ok(msg("已打开程序文件位置"))
                        },
                        cx,
                    );
                }
            }
            ProcessRowAction::CopyCommandLine => {
                let Some(ticks) = target.creation_time_windows_ticks else {
                    return;
                };
                let pid = target.pid;
                cx.spawn(async move |view, cx| {
                    let result = smol::unblock(move || {
                        ProcessDetails::read(pid, ticks)?
                            .command_line
                            .context(trs("Windows 未提供此进程的命令行"))
                    })
                    .await;
                    let _ = view.update(cx, |this, cx| match result {
                        Ok(command) => this.copy_developer_text(
                            command,
                            mf(" PID {pid} 的命令行", &[("pid", format!("{pid}"))]),
                            cx,
                        ),
                        Err(error) => {
                            this.message = Some(mf(
                                "复制命令行失败：{error:#}",
                                &[("error", format!("{error:#}"))],
                            ));
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            ProcessRowAction::EndProcess
            | ProcessRowAction::EndProcessTree
            | ProcessRowAction::EndApplication => {
                let Some(ticks) = target
                    .creation_time_windows_ticks
                    .filter(|ticks| *ticks >= 116_444_736_000_000_000)
                else {
                    self.message = Some(msg("此进程缺少精确创建时间，无法安全结束；请刷新后重试"));
                    cx.notify();
                    return;
                };
                let pid = target.pid;
                let tree = matches!(request.action, ProcessRowAction::EndProcessTree);
                let application = matches!(request.action, ProcessRowAction::EndApplication);
                let label = if application {
                    "结束应用"
                } else if tree {
                    "结束进程树"
                } else {
                    "结束进程"
                };
                let identities = target.application_targets;
                if application && identities.is_none() {
                    return;
                }
                let description = if application {
                    tf(
                        "结束 {} 的 {} 个已捕获成员？新产生的子进程不在此次名单中。未保存的数据可能丢失。",
                        &[
                            ("arg0", target.name.to_string()),
                            (
                                "arg1",
                                format!("{}", identities.as_ref().map_or(0, Vec::len)),
                            ),
                        ],
                    )
                } else {
                    tf(
                        "{label} {}（PID {pid}）？未保存的数据可能丢失。",
                        &[
                            ("label", trs(label)),
                            ("arg0", target.name.to_string()),
                            ("pid", format!("{pid}")),
                        ],
                    )
                };
                self.confirm_management(
                    label,
                    description,
                    true,
                    move || {
                        if let Some(identities) = &identities
                            && application
                        {
                            let ended = platform::terminate_application_processes(identities)?;
                            return Ok(mf(
                                "已结束 {} 个应用进程",
                                &[("arg0", format!("{}", ended.len()))],
                            ));
                        }
                        if tree {
                            let ended = platform::terminate_process_tree(pid, ticks)?;
                            Ok(mf(
                                "已结束 {} 个进程",
                                &[("arg0", format!("{}", ended.len()))],
                            ))
                        } else {
                            platform::terminate_process(pid, ticks)?;
                            Ok(mf("已结束 PID {pid}", &[("pid", format!("{pid}"))]))
                        }
                    },
                    window,
                    cx,
                );
            }
        }
    }

    fn clear_process_selection_for_navigation(&mut self, cx: &mut Context<Self>) {
        self.processes.update(cx, |table, cx| {
            table.delegate_mut().selected = None;
            table.clear_selection(cx);
        });
        self.process_details_identity = None;
        self.process_details = None;
        self.process_details_error = None;
        self.process_details_loading = false;
        self.process_details_generation = self.process_details_generation.wrapping_add(1);
        self.file_signature = None;
        self.file_signature_loading = false;
        self.file_signature_error = None;
        self.file_signature_generation = self.file_signature_generation.wrapping_add(1);
    }

    pub(super) fn handle_port_action(
        &mut self,
        event: ports::PortMenuEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use ports::PortMenuAction;
        let entry = event.entry;
        match event.action {
            PortMenuAction::CopyPid => {
                self.copy_developer_text(entry.pid.to_string(), " PID".to_owned().into(), cx)
            }
            PortMenuAction::CopyLocal => {
                self.copy_developer_text(entry.local.to_string(), msg("本地端点"), cx)
            }
            PortMenuAction::CopyRemote => {
                if let Some(remote) = entry.remote {
                    self.copy_developer_text(remote.to_string(), msg("远端端点"), cx);
                }
            }
            PortMenuAction::CopyProcessPath => {
                if let Some(path) = entry.exe {
                    self.copy_developer_text(
                        path.to_string_lossy().into_owned(),
                        msg("程序路径"),
                        cx,
                    );
                }
            }
            PortMenuAction::ShowProcess => {
                self.clear_process_selection_for_navigation(cx);
                self.set_page(Page::Details, window, cx);
                self.search.update(cx, |input, cx| {
                    input.set_value(format!("pid:{}", entry.pid), window, cx)
                });
                self.filter(cx);
                let selected = self.processes.read(cx).delegate();
                let row = (0..selected.rows.len()).find(|index| {
                    selected.row(*index).is_some_and(|process| {
                        process.pid == entry.pid
                            && process.start_time_windows_ticks == entry.creation_ticks
                    })
                });
                if let Some(row) = row {
                    self.processes.update(cx, |table, cx| {
                        table.delegate_mut().selected =
                            table.delegate().row(row).map(tables::process_identity);
                        table.set_selected_row(row, cx);
                    });
                    if let Some(ticks) = entry.creation_ticks {
                        self.refresh_process_details(entry.pid, ticks, cx);
                    }
                } else {
                    self.message = Some(mf(
                        "PID {} 的原始实例未出现在最新进程快照；等待下一次采样",
                        &[("arg0", format!("{}", entry.pid))],
                    ));
                    cx.notify();
                }
            }
            PortMenuAction::EndProcess => {
                if !entry.can_end_process() {
                    self.message = Some(msg("端点所有者未取得可操作的精确身份，或属于受保护进程"));
                    cx.notify();
                    return;
                }
                self.handle_process_action(
                    ProcessActionRequest {
                        action: ProcessRowAction::EndProcess,
                        target: tables::ProcessRowTarget {
                            pid: entry.pid,
                            creation_time_windows_ticks: entry.creation_ticks,
                            name: entry
                                .process_name
                                .unwrap_or_else(|| format!("PID {}", entry.pid)),
                            exe: entry.exe,
                            application_targets: None,
                        },
                    },
                    window,
                    cx,
                );
            }
        }
    }

    pub(super) fn refresh_ports(&mut self, cx: &mut Context<Self>) {
        if self.ports_loading {
            return;
        }
        self.ports_loading = true;
        self.ports_last_refresh = Some(std::time::Instant::now());
        let processes = self.snapshot.clone();
        cx.spawn(async move |view, cx| {
            let result = smol::unblock(move || {
                Arc::new(PortSnapshot::collect(
                    processes
                        .as_ref()
                        .map(|snapshot| snapshot.processes.as_slice())
                        .unwrap_or(&[]),
                ))
            })
            .await;
            let _ = view.update(cx, |this, cx| {
                this.ports_loading = false;
                this.ports_snapshot = Some(result.clone());
                this.ports.update(cx, |table, cx| {
                    table.delegate_mut().set_snapshot(result);
                    ports::sync_selection(table, cx);
                    cx.notify();
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn ports_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.ports.read(cx).delegate().rows.len();
        let total = self
            .ports_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.entries.len());
        v_flex().size_full().min_h_0()
            .child(h_flex().px_6().py_3().gap_3().justify_between()
                .child(div().text_sm().child(tf("{total} 个端点 · {rows} 条匹配 · 名称 A–Z / 同名 PID", &[("total", format!("{total}")), ("rows", format!("{rows}"))])))
                .child(h_flex().gap_2()
                    .child(Button::new("ports-export").ghost().small().label(tr("导出 CSV"))
                        .disabled(self.ports_snapshot.is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.export_ports(cx))))
                    .child(Button::new("refresh-ports").small().label(tr(if self.ports_loading { "读取中…" } else { "刷新端口" }))
                        .disabled(self.ports_loading)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_ports(cx))))))
            .child(div().px_6().pb_2().text_xs().text_color(cx.theme().muted_foreground)
                .child(tr("搜索示例：port:8080、pid:1234、tcp listen、udp ipv6；多个条件以空格分隔。右键可定位或结束占用进程。")))
            .when_some(self.ports_snapshot.as_ref(), |element, snapshot| {
                element
                    .children(snapshot.issues.iter().take(4).map(|issue| div().px_6().pb_1().text_xs().text_color(cx.theme().muted_foreground).child(issue.clone())))
                    .child(div().px_6().pb_2().text_xs().text_color(cx.theme().muted_foreground)
                        .child(tf("Windows IP Helper · 最近读取 {} ms · 仅页面打开时每 2 秒刷新；UDP 不提供远端地址或连接状态。", &[("arg0", format!("{}", snapshot.collection_duration_ms))])))
            })
            .child(div().flex_1().min_h_0().child(DataTable::new(&self.ports).bordered(false).stripe(false).small()))
            .into_any_element()
    }

    pub(super) fn export_ports(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.ports_snapshot.clone() else {
            return;
        };
        let query = self.search.read(cx).value().to_string();
        let indices = self.ports.read(cx).delegate().rows.clone();
        let directory =
            PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_else(|| ".".into()));
        let prompt = cx.prompt_for_new_path(&directory, Some("SystemMonitorX-ports.csv"));
        cx.spawn(async move |view, cx| {
            if let Ok(Ok(Some(path))) = prompt.await {
                let result = smol::unblock(move || -> anyhow::Result<PathBuf> {
                    let csv = port_csv(&snapshot, &indices, &query)?;
                    std::fs::write(&path, csv)?;
                    Ok(path)
                })
                .await;
                let _ = view.update(cx, |this, cx| {
                    this.message = Some(match result {
                        Ok(path) => mf(
                            "已导出端口查询 {}",
                            &[("arg0", format!("{}", path.display()))],
                        ),
                        Err(error) => mf(
                            "端口导出失败：{error:#}",
                            &[("error", format!("{error:#}"))],
                        ),
                    });
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

/// A captured port query exports only its captured rows, with an English schema.
fn port_csv(snapshot: &PortSnapshot, indices: &[usize], query: &str) -> anyhow::Result<String> {
    let mut csv = String::from(
        "\u{feff}Process name,PID,Protocol,Address family,Local endpoint,Remote endpoint,State,Executable,Creation FILETIME,Query,Identity note\r\n",
    );
    for &index in indices {
        let entry = snapshot.entries.get(index).context(trs("端口快照行无效"))?;
        let fields = [
            entry.process_name.clone().unwrap_or_default(),
            entry.pid.to_string(),
            entry.protocol.to_string(),
            entry.family.to_string(),
            entry.local.to_string(),
            entry
                .remote
                .map(|remote| remote.to_string())
                .unwrap_or_default(),
            entry
                .tcp_state
                .as_ref()
                .map(|state| state.to_string())
                .unwrap_or_default(),
            entry
                .exe
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            entry
                .creation_ticks
                .map(|ticks| ticks.to_string())
                .unwrap_or_default(),
            query.to_owned(),
            entry.identity_note.clone().unwrap_or_default(),
        ];
        csv.push_str(
            &fields
                .iter()
                .map(|field| csv_cell(field))
                .collect::<Vec<_>>()
                .join(","),
        );
        csv.push_str("\r\n");
        ensure!(
            csv.len() <= 32 * 1024 * 1024,
            trs("端口 CSV 超过 32 MiB 上限")
        );
    }
    ensure!(
        csv.len() <= 32 * 1024 * 1024,
        trs("端口 CSV 超过 32 MiB 上限")
    );
    Ok(csv)
}

/// Preserve literal command/path cells when opened by spreadsheet applications.
fn csv_cell(value: &str) -> String {
    let first = value
        .trim_start_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
        .chars()
        .next();
    let guarded =
        if matches!(first, Some('=' | '+' | '-' | '@')) || value.starts_with(['\t', '\r', '\n']) {
            format!("'{value}")
        } else {
            value.to_owned()
        };
    format!("\"{}\"", guarded.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::{csv_cell, port_csv};
    use system_monitor_x::network_ports::{IpFamily, PortEntry, PortProtocol, PortSnapshot};
    #[test]
    fn port_csv_uses_an_english_schema_and_keeps_filtered_raw_identity_notes() {
        let fixture = PortEntry {
            pid: 42,
            creation_ticks: None,
            process_name: Some("服务-é-日本.exe".into()),
            exe: Some("C:/国际/app.exe".into()),
            protocol: PortProtocol::Udp,
            family: IpFamily::V6,
            local: "[::1]:8080".parse().unwrap(),
            remote: None,
            tcp_state: None,
            identity_note: Some("身份不可用, %{pid}".into()),
        };
        let other = PortEntry {
            process_name: Some("unrelated.exe".into()),
            ..fixture.clone()
        };
        let snapshot = PortSnapshot {
            entries: vec![other, fixture],
            ..Default::default()
        };
        let csv = port_csv(&snapshot, &[1], "=query").unwrap();
        assert!(csv.starts_with("\u{feff}Process name,PID,Protocol,Address family,Local endpoint,Remote endpoint,State,Executable,Creation FILETIME,Query,Identity note\r\n"));
        assert!(!csv.contains("unrelated.exe"));
        assert_eq!(csv.lines().count(), 2);
        assert!(csv.contains(r#""服务-é-日本.exe","42","UDP","IPv6","[::1]:8080","","","C:/国际/app.exe","","'=query","身份不可用, %{pid}""#));
        assert!(port_csv(&snapshot, &[99], "").is_err());
    }
    #[test]
    fn port_csv_preserves_literal_paths_and_prevents_formulas() {
        assert_eq!(csv_cell("=cmd"), "\"'=cmd\"");
        assert_eq!(csv_cell("C:\\a,\"x\""), "\"C:\\a,\"\"x\"\"\"");
        assert_eq!(csv_cell("127.0.0.1:8080"), "\"127.0.0.1:8080\"");
    }
}
