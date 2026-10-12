use super::*;
use anyhow::{Context as _, Result, ensure};
use system_monitor_x::{
    monitor::ProcessSample,
    platform::{ProcessControls, ServiceAction, ServiceInfo, StartupEntry, UserSessionAction},
    sessions::SessionSample,
};

#[derive(Clone, Copy)]
enum ProcessDialog {
    Affinity,
    Efficiency,
}

fn matching_service<'a>(entries: &'a [ServiceInfo], row: &[String]) -> Option<&'a ServiceInfo> {
    let name = row.first()?;
    entries.iter().find(|entry| entry.name == *name)
}

fn matching_startup<'a>(entries: &'a [StartupEntry], row: &[String]) -> Option<&'a StartupEntry> {
    let (name, command, source) = (row.first()?, row.get(2)?, row.get(3)?);
    entries
        .iter()
        .find(|entry| entry.name == *name && entry.command == *command && entry.source == *source)
}

fn matching_session<'a>(
    sessions: &'a [SessionSample],
    row: &[String],
) -> Option<&'a SessionSample> {
    let id = row.get(1)?.parse::<u32>().ok()?;
    sessions.iter().find(|session| {
        session.id == id
            && row.first().is_some_and(|name| {
                *name
                    == format!(
                        "{}\\{}",
                        session.domain.as_deref().unwrap_or(""),
                        session.user_name.as_deref().unwrap_or("")
                    )
            })
    })
}

fn affinity_mask(value: &str, available: usize) -> Result<usize> {
    let value = value.trim();
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    let mask = usize::from_str_radix(value, 16).context("请输入十六进制掩码，例如 0x3")?;
    ensure!(mask != 0, "至少选择一个 CPU，掩码不能为 0");
    ensure!(mask & !available == 0, "掩码包含当前处理器组中不可用的 CPU");
    Ok(mask)
}

impl SystemMonitorApp {
    /// Match the selected visible row against the captured Windows identity,
    /// rather than indexing the unsorted metadata vector.
    fn selected_service(&self, cx: &App) -> Option<ServiceInfo> {
        let table = self.services.read(cx);
        let row = table.delegate().rows.get(table.selected_row()?)?;
        matching_service(&self.service_entries, row).cloned()
    }

    fn selected_startup(&self, cx: &App) -> Option<StartupEntry> {
        let table = self.startup.read(cx);
        let row = table.delegate().rows.get(table.selected_row()?)?;
        matching_startup(&self.startup_entries, row).cloned()
    }

    fn selected_session(&self, cx: &App) -> Option<SessionSample> {
        let table = self.users.read(cx);
        let row = table.delegate().rows.get(table.selected_row()?)?;
        matching_session(&self.snapshot.as_ref()?.sessions, row).cloned()
    }

    pub(super) fn confirm_management(
        &mut self,
        title: &str,
        description: String,
        danger: bool,
        operation: impl Fn() -> Result<String> + Send + Sync + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.operation_busy {
            return;
        }
        let title = title.to_owned();
        let operation = Arc::new(operation);
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            dialog
                .confirm()
                .title(title.clone())
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(title.clone())
                        .cancel_text("取消")
                        .show_cancel(true)
                        .ok_variant(if danger {
                            ButtonVariant::Danger
                        } else {
                            ButtonVariant::Primary
                        }),
                )
                .on_ok({
                    let view = view.clone();
                    let operation = operation.clone();
                    let label = title.clone();
                    move |_, _, cx| {
                        let operation = operation.clone();
                        view.update(cx, |this, cx| {
                            if this.operation_busy {
                                return false;
                            }
                            this.run_operation(&label, move || operation(), cx);
                            true
                        })
                        .unwrap_or(false)
                    }
                })
        });
    }

    pub(super) fn management_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.page == Page::Services {
            let selected = self.selected_service(cx);
            let allowed = !self.operation_busy
                && !self.metadata_loading
                && selected
                    .as_ref()
                    .is_some_and(|service| service.configuration.is_some());
            let stopped = selected
                .as_ref()
                .is_some_and(|service| service.status == "已停止");
            let running = selected
                .as_ref()
                .is_some_and(|service| matches!(service.status.as_str(), "运行中" | "已暂停"));
            h_flex()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .max_w(px(240.))
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            selected
                                .as_ref()
                                .map(|s| format!("{} · {}", s.display_name, s.status))
                                .unwrap_or_else(|| "选择服务后操作".into()),
                        ),
                )
                .children(
                    [
                        (
                            "service-start",
                            "启动",
                            ServiceAction::Start,
                            allowed && stopped,
                        ),
                        (
                            "service-stop",
                            "停止",
                            ServiceAction::Stop,
                            allowed && running,
                        ),
                        (
                            "service-restart",
                            "重启",
                            ServiceAction::Restart,
                            allowed && running,
                        ),
                    ]
                    .into_iter()
                    .map(|(id, label, action, enabled)| {
                        Button::new(id)
                            .small()
                            .label(label)
                            .disabled(!enabled)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(service) = this.selected_service(cx) {
                                    let description = format!(
                                        "{label}服务 {}（{}）？依赖它的应用可能受到影响。",
                                        service.display_name, service.name
                                    );
                                    this.confirm_management(
                                        label,
                                        description,
                                        !matches!(action, ServiceAction::Start),
                                        move || {
                                            platform::control_service(&service, action)?;
                                            Ok(format!(
                                                "服务 {} 操作完成，请查看最新状态",
                                                service.display_name
                                            ))
                                        },
                                        window,
                                        cx,
                                    );
                                }
                            }))
                    }),
                )
                .into_any_element()
        } else {
            let selected = self.selected_startup(cx);
            let enabled = selected.as_ref().and_then(|entry| entry.enabled);
            h_flex()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .max_w(px(230.))
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            selected
                                .as_ref()
                                .map(|entry| {
                                    format!(
                                        "{} · {}",
                                        entry.name,
                                        enabled
                                            .map(|v| if v { "已启用" } else { "已禁用" })
                                            .unwrap_or("状态未知")
                                    )
                                })
                                .unwrap_or_else(|| "选择启动应用后操作".into()),
                        ),
                )
                .child(
                    Button::new("startup-toggle")
                        .small()
                        .label(if enabled == Some(true) {
                            "禁用"
                        } else {
                            "启用"
                        })
                        .disabled(enabled.is_none() || self.operation_busy || self.metadata_loading)
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(entry) = this.selected_startup(cx)
                                && let Some(previous) = entry.enabled
                            {
                                let label = if previous {
                                    "禁用启动应用"
                                } else {
                                    "启用启动应用"
                                };
                                let description = format!(
                                    "{}（{}）将在下次登录时{}启动。当前运行的程序不受影响。",
                                    entry.name,
                                    entry.source,
                                    if previous { "不再自动" } else { "自动" }
                                );
                                this.confirm_management(
                                    label,
                                    description,
                                    false,
                                    move || {
                                        platform::set_startup_enabled(&entry, !previous)?;
                                        Ok(format!(
                                            "启动应用 {} 已{}",
                                            entry.name,
                                            if previous { "禁用" } else { "启用" }
                                        ))
                                    },
                                    window,
                                    cx,
                                );
                            }
                        })),
                )
                .into_any_element()
        }
    }

    pub(super) fn users_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let sessions = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.sessions.as_slice())
            .unwrap_or(&[]);
        let users: Vec<_> = sessions
            .iter()
            .filter(|s| s.user_name.as_ref().is_some_and(|u| !u.is_empty()))
            .collect();
        let selected = self.selected_session(cx);
        let valid = !self.operation_busy
            && selected.as_ref().is_some_and(|session| {
                session.id != 0
                    && session
                        .logon_time_windows_ticks
                        .is_some_and(|ticks| ticks > 0)
                    && session
                        .user_name
                        .as_ref()
                        .is_some_and(|name| !name.is_empty())
            });
        let summary = format!(
            "{} 个登录会话 · {} 个进程 · CPU {} · 工作集合计 {}",
            users.len(),
            users.iter().map(|s| s.process_count).sum::<usize>(),
            if self.snapshot.as_ref().is_some_and(|s| s.cpu_ready) {
                format!("{:.1}%", users.iter().map(|s| s.cpu_usage).sum::<f32>())
            } else {
                "—".into()
            },
            bytes(users.iter().map(|s| s.memory_bytes).sum())
        );
        v_flex().size_full().min_h_0()
            .child(h_flex().h(px(52.)).px_6().justify_between().gap_3()
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(summary))
                .child(h_flex().gap_2().children([
                    ("session-disconnect", "断开", UserSessionAction::Disconnect),
                    ("session-logoff", "注销", UserSessionAction::Logoff),
                ].into_iter().map(|(id, label, action)| {
                    Button::new(id).small().label(label).disabled(!valid)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(session) = this.selected_session(cx) {
                                let user = session.user_name.clone().unwrap_or_default();
                                let domain = session.domain.clone().unwrap_or_default();
                                let Some(logon) = session.logon_time_windows_ticks else { return; };
                                let description = format!("{label}用户 {domain}\\{user}（会话 {}）？{}", session.id,
                                    match action { UserSessionAction::Disconnect => "应用将继续运行，远程或本地连接将断开。",
                                        UserSessionAction::Logoff => "该会话的所有应用将关闭，未保存的数据可能丢失。" });
                                this.confirm_management(label, description, matches!(action, UserSessionAction::Logoff),
                                    move || { platform::control_user_session(session.id, &user, &domain, logon, action)?;
                                        Ok(format!("已提交{label}请求，请查看最新会话状态")) }, window, cx);
                            }
                        }))
                }))))
            .when_some(self.snapshot.as_ref().and_then(|s| s.sessions_note.clone()), |e, note| {
                e.child(div().px_6().py_2().text_xs().text_color(cx.theme().muted_foreground).child(note))
            })
            .child(div().px_6().pb_2().text_xs().text_color(cx.theme().muted_foreground)
                .child("CPU 按整机为 100% 统计；工作集包含共享页，跨进程合计会重复计算共享部分；GPU 显示最繁忙进程。"))
            .child(div().flex_1().min_h_0().child(DataTable::new(&self.users).bordered(false).stripe(false).small()))
            .into_any_element()
    }

    pub(super) fn app_history_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let history = self
            .snapshot
            .as_ref()
            .map(|s| s.app_history.as_slice())
            .unwrap_or(&[]);
        let now = self
            .snapshot
            .as_ref()
            .and_then(|s| s.sampled_at.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let earliest = history
            .iter()
            .map(|s| s.first_seen_unix_secs)
            .filter(|n| *n > 0)
            .min();
        let scope = earliest
            .map(|earliest| {
                format!(
                    "{} 个应用 · 最早观察记录距今 {}",
                    history.len(),
                    duration(now.saturating_sub(earliest))
                )
            })
            .unwrap_or_else(|| "等待应用资源历史…".into());
        v_flex().size_full().min_h_0()
            .child(h_flex().px_6().py_3().justify_between().child(div().text_sm().child(scope))
                .child(Button::new("clear-history").ghost().small().label("清空历史…").disabled(self.operation_busy).on_click(cx.listener(|this,_,window,cx| {
                    let commands = this.collector_commands.clone();
                    this.confirm_management("清空资源历史", "删除本机保存的应用资源历史，并从下一次采样重新统计？".into(), true, move || {
                        let (reply,receiver)=std::sync::mpsc::channel();
                        commands.try_send(CollectorCommand::ResetHistory(reply)).map_err(|e|anyhow::anyhow!("采集命令未提交：{e}"))?;
                        receiver.recv_timeout(Duration::from_secs(10)).map_err(|e|anyhow::anyhow!("等待清理历史失败：{e}"))?.map_err(anyhow::Error::msg)?;
                        Ok("历史已清空，从下一次采样重新统计".into())
                    },window,cx);
                }))))
            .child(div().px_6().pb_3().text_xs().text_color(cx.theme().muted_foreground)
                .child("累计 CPU 时间与 I/O 读写从首次观察开始计算，历史保存在本机。网络累计仅计完整、无丢失的 ETW 周期；网络有效进程秒是各实例有效覆盖时长之和，缺权限/丢事件的缺口不计。I/O 包含文件、设备和其他读取/写入，不能当作应用网络流量。"))
            .when_some(self.snapshot.as_ref().and_then(|s| s.history_note.clone()), |e, note| {
                e.child(div().px_6().pb_3().text_xs().text_color(cx.theme().muted_foreground).child(note))
            })
            .child(div().flex_1().min_h_0().child(DataTable::new(&self.app_history).bordered(false).stripe(false).small()))
            .into_any_element()
    }

    pub(super) fn extra_process_controls(
        &self,
        p: ProcessSample,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let available = p.start_time_windows_ticks.is_some_and(|ticks| ticks > 0)
            && p.pid != 0
            && p.pid != 4
            && p.pid != std::process::id()
            && !self.operation_busy;
        let number =
            |value: Option<u32>| value.map(|n| n.to_string()).unwrap_or_else(|| "—".into());
        let description = format!(
            "线程 {} · 句柄 {} · 用户 {} · 会话 {} · 私有提交 {} · CPU 时间 {}",
            number(p.thread_count),
            number(p.handle_count),
            p.user_name.as_deref().unwrap_or("—"),
            number(p.session_id),
            p.private_bytes.map(bytes).unwrap_or_else(|| "—".into()),
            p.cpu_time_seconds
                .map(|seconds| format!("{seconds:.2} s"))
                .unwrap_or_else(|| "—".into())
        );
        v_flex().gap_2()
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(description))
            .child(h_flex().gap_2()
                .children([
                    ("process-efficiency", "效率模式…", ProcessDialog::Efficiency),
                    ("process-affinity", "CPU 亲和性…", ProcessDialog::Affinity),
                ].into_iter().map(|(id, label, kind)| {
                    let process = p.clone();
                    Button::new(id).small().ghost().label(label).disabled(!available)
                        .on_click(cx.listener(move |this, _, window, cx| this.load_process_controls(process.clone(), kind, window, cx)))
                }))
                .child(Button::new("process-end-tree").small().ghost().label("结束进程树…").disabled(!available)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let (pid, start) = (p.pid, p.start_time_windows_ticks.unwrap_or(0));
                        this.confirm_management("结束进程树", format!("结束 {}（PID {pid}）及其当前子进程？所有目标都会重新核验身份。未保存的数据可能丢失。", p.name),
                            true, move || {
                                let ended = platform::terminate_process_tree(pid, start)?;
                                Ok(format!("已结束 {} 个进程：{ended:?}", ended.len()))
                            }, window, cx);
                    }))))
            .into_any_element()
    }

    fn load_process_controls(
        &mut self,
        process: ProcessSample,
        kind: ProcessDialog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.operation_busy {
            return;
        }
        self.operation_busy = true;
        self.message = Some("正在读取进程设置…".into());
        let (pid, start) = (process.pid, process.start_time_windows_ticks.unwrap_or(0));
        cx.spawn_in(window, async move |view, cx| {
            let result = smol::unblock(move || platform::process_controls(pid, start)).await;
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.operation_busy = false;
                    match result {
                        Ok(controls) => {
                            this.message = None;
                            match kind {
                                ProcessDialog::Affinity => this.affinity_dialog(process, controls, window, cx),
                                ProcessDialog::Efficiency => {
                                    let enabled = !controls.efficiency_mode;
                                    let label = if enabled { "启用效率模式" } else { "关闭效率模式" };
                                    this.confirm_management(label,
                                        format!("{}（PID {}）当前效率模式{}。{}", process.name, process.pid,
                                            if controls.efficiency_mode { "已启用" } else { "未启用" },
                                            if enabled { "启用后将降低调度优先级并使用 EcoQoS，后台任务可能更慢。" }
                                                else { "关闭后取消 EcoQoS 限制，并恢复空闲优先级为正常。" }),
                                        false, move || { platform::set_efficiency_mode(pid, start, enabled)?;
                                            Ok(format!("效率模式已{}", if enabled { "启用" } else { "关闭" })) }, window, cx);
                                }
                            }
                        }
                        Err(error) => this.message = Some(format!("进程设置不可用：{error:#}")),
                    }
                    cx.notify();
                });
            });
        }).detach();
        cx.notify();
    }

    fn affinity_dialog(
        &self,
        process: ProcessSample,
        controls: ProcessControls,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(format!("0x{:X}", controls.affinity_mask))
                .placeholder("例如 0x3")
        });
        let error = cx.new(|_| String::new());
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog.title("CPU 亲和性").width(px(520.))
                .child(v_flex().gap_3()
                    .child(format!("{} · PID {} · 处理器组 {}", process.name, process.pid, controls.processor_group))
                    .child(div().text_sm().child(format!("可用掩码 0x{:X}；当前掩码 0x{:X}", controls.system_affinity_mask, controls.affinity_mask)))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground)
                        .child("使用十六进制掩码：每一位对应该处理器组的一个逻辑 CPU，1 表示允许使用。例如 0x3 选择 CPU 0 和 1。"))
                    .child(Input::new(&input))
                    .child(div().text_sm().text_color(rgb(0xf87171)).child(error.read(cx).clone())))
                .button_props(DialogButtonProps::default().ok_text("应用亲和性").cancel_text("取消").show_cancel(true))
                .on_ok({
                    let input = input.clone();
                    let error = error.clone();
                    let view = view.clone();
                    let (pid, start, available) = (process.pid, process.start_time_windows_ticks.unwrap_or(0), controls.system_affinity_mask);
                    move |_, window, cx| {
                        let mask = match affinity_mask(&input.read(cx).value(), available) {
                            Ok(mask) => mask,
                            Err(problem) => {
                                error.update(cx, |message, cx| { *message = format!("{problem:#}"); cx.notify(); });
                                window.refresh();
                                return false;
                            }
                        };
                        view.update(cx, |this, cx| {
                            if this.operation_busy { return false; }
                            this.run_operation("设置 CPU 亲和性", move || {
                                platform::set_affinity(pid, start, mask)?;
                                Ok(format!("CPU 亲和性已设为 0x{mask:X}"))
                            }, cx);
                            true
                        }).unwrap_or(false)
                    }
                })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{affinity_mask, matching_service, matching_session};
    use system_monitor_x::{platform::ServiceInfo, sessions::SessionSample};

    #[test]
    fn affinity_hex_validation_rejects_empty_zero_and_unavailable_cpus() {
        assert_eq!(affinity_mask(" 0x3 ", 0xf).unwrap(), 3);
        assert_eq!(affinity_mask("0Xf", 0xf).unwrap(), 15);
        assert!(affinity_mask("0", 0xf).is_err());
        assert!(affinity_mask("", 0xf).is_err());
        assert!(affinity_mask("invalid", 0xf).is_err());
        assert!(affinity_mask("10", 0xf).is_err());
    }

    #[test]
    fn management_selection_uses_row_identity_after_sorting() {
        let service = |name: &str| ServiceInfo {
            name: name.into(),
            display_name: name.into(),
            status: "运行中".into(),
            pid: 1,
            start_type: "手动".into(),
            configuration: None,
        };
        let entries = vec![service("z"), service("a")];
        assert_eq!(matching_service(&entries, &["a".into()]).unwrap().name, "a");
        assert!(matching_service(&entries, &["missing".into()]).is_none());
        let session = SessionSample {
            id: 17,
            user_name: Some("fixture".into()),
            domain: Some("TEST".into()),
            logon_time_windows_ticks: Some(100),
            ..Default::default()
        };
        let sessions = vec![session];
        assert_eq!(
            matching_session(&sessions, &["TEST\\fixture".into(), "17".into()])
                .unwrap()
                .id,
            17
        );
        assert!(matching_session(&sessions, &["TEST\\changed".into(), "17".into()]).is_none());
        assert!(matching_session(&sessions, &[]).is_none());
    }
}
