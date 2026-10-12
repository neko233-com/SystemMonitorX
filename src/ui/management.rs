use super::*;
use anyhow::{Context as _, Result, ensure};
use system_monitor_x::{
    monitor::ProcessSample,
    platform::{
        ProcessControls, ServiceAction, ServiceInfo, ServiceState, StartupEntry, StartupSource,
        UserSessionAction,
    },
    sessions::SessionSample,
};

#[derive(Clone, Copy)]
enum ProcessDialog {
    Affinity,
    Efficiency,
}

pub(super) fn service_state_text(state: ServiceState) -> String {
    let source = match state {
        ServiceState::Stopped => "已停止",
        ServiceState::StartPending => "正在启动",
        ServiceState::StopPending => "正在停止",
        ServiceState::Running => "运行中",
        ServiceState::ContinuePending => "正在继续",
        ServiceState::PausePending => "正在暂停",
        ServiceState::Paused => "已暂停",
        ServiceState::Unknown(code) => {
            return tf("未知状态 {code}", &[("code", code.to_string())]);
        }
    };
    trs(source)
}

pub(super) fn startup_source_text(source: StartupSource) -> String {
    trs(match source {
        StartupSource::Registry {
            machine: false,
            view32: false,
        } => "HKCU · Run",
        StartupSource::Registry {
            machine: true,
            view32: false,
        } => "HKLM · Run",
        StartupSource::Registry {
            machine: false,
            view32: true,
        } => "HKCU · Run (32 位)",
        StartupSource::Registry {
            machine: true,
            view32: true,
        } => "HKLM · Run (32 位)",
        StartupSource::Folder { machine: false } => "当前用户启动文件夹",
        StartupSource::Folder { machine: true } => "所有用户启动文件夹",
    })
}

fn startup_change_message(name: &str, was_enabled: bool) -> UiMessage {
    mf(
        if was_enabled {
            "启动应用 {} 已禁用"
        } else {
            "启动应用 {} 已启用"
        },
        &[("arg0", name.to_owned())],
    )
}

fn efficiency_change_message(enabled: bool) -> UiMessage {
    msg(if enabled {
        "效率模式已启用"
    } else {
        "效率模式已关闭"
    })
}

fn session_action_message(label: &str) -> UiMessage {
    mlf(
        "已提交{label}请求，请查看最新会话状态",
        &[("label", msg(label))],
    )
}

fn matching_service<'a>(entries: &'a [ServiceInfo], row: &[String]) -> Option<&'a ServiceInfo> {
    let name = row.first()?;
    entries.iter().find(|entry| entry.name == *name)
}

fn unique_matching<T>(entries: &[T], matches: impl Fn(&T) -> bool) -> Option<&T> {
    let mut found = None;
    for entry in entries {
        if matches(entry) {
            if found.is_some() {
                return None;
            }
            found = Some(entry);
        }
    }
    found
}

fn startup_row_matches(
    name: &str,
    command: &str,
    source: &str,
    selection_key: impl FnOnce() -> String,
    row: &[String],
) -> bool {
    if row.first().is_none_or(|value| value != name)
        || row.get(2).is_none_or(|value| value != command)
    {
        return false;
    }
    if let Some(key) = row.get(4) {
        // Once a raw key is present, stale/empty keys must never fall back to
        // a translated label or another entry with the same visible columns.
        !key.is_empty() && *key == selection_key()
    } else {
        row.get(3).is_some_and(|value| value == source)
    }
}

fn matching_startup<'a>(entries: &'a [StartupEntry], row: &[String]) -> Option<&'a StartupEntry> {
    unique_matching(entries, |entry| {
        startup_row_matches(
            &entry.name,
            &entry.command,
            &entry.source,
            || entry.selection_key(),
            row,
        )
    })
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
    let mask = usize::from_str_radix(value, 16).context(trs("请输入十六进制掩码，例如 0x3"))?;
    ensure!(mask != 0, "{}", trs("至少选择一个 CPU，掩码不能为 0"));
    ensure!(
        mask & !available == 0,
        "{}",
        trs("掩码包含当前处理器组中不可用的 CPU")
    );
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
        operation: impl FnOnce() -> Result<UiMessage> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.operation_busy {
            return;
        }
        let label_source = title.to_owned();
        let title = trs(title);
        // Dialogs may rebuild their callbacks, but an accepted operation is single-use.
        let operation = Arc::new(Mutex::new(Some(operation)));
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            dialog
                .confirm()
                .title(title.clone())
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(title.clone())
                        .cancel_text(tr("取消"))
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
                    let label = label_source.clone();
                    move |_, _, cx| {
                        let operation = operation.clone();
                        view.update(cx, |this, cx| {
                            if this.operation_busy {
                                return false;
                            }
                            let Some(operation) =
                                operation.lock().ok().and_then(|mut action| action.take())
                            else {
                                return false;
                            };
                            this.run_operation(&label, operation, cx);
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
                .is_some_and(|service| service.state.allows_action(ServiceAction::Start));
            let running = selected
                .as_ref()
                .is_some_and(|service| service.state.allows_action(ServiceAction::Stop));
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
                                .map(|s| {
                                    format!("{} · {}", s.display_name, service_state_text(s.state))
                                })
                                .unwrap_or_else(|| trs("选择服务后操作")),
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
                            .label(tr(label))
                            .disabled(!enabled)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(service) = this.selected_service(cx) {
                                    let description = tf(
                                        "{label}服务 {}（{}）？依赖它的应用可能受到影响。",
                                        &[
                                            ("label", trs(label)),
                                            ("arg0", service.display_name.to_string()),
                                            ("arg1", service.name.to_string()),
                                        ],
                                    );
                                    this.confirm_management(
                                        label,
                                        description,
                                        !matches!(action, ServiceAction::Start),
                                        move || {
                                            platform::control_service(&service, action)?;
                                            Ok(mf(
                                                "服务 {} 操作完成，请查看最新状态",
                                                &[("arg0", service.display_name.to_string())],
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
                                        trs(match enabled {
                                            Some(true) => "已启用",
                                            Some(false) => "已禁用",
                                            None => "状态未知",
                                        })
                                    )
                                })
                                .unwrap_or_else(|| trs("选择启动应用后操作")),
                        ),
                )
                .child(
                    Button::new("startup-toggle")
                        .small()
                        .label(tr(if enabled == Some(true) {
                            "禁用"
                        } else {
                            "启用"
                        }))
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
                                let description = tf(if previous { "{}（{}）将在下次登录时不再自动启动。当前运行的程序不受影响。" } else { "{}（{}）将在下次登录时自动启动。当前运行的程序不受影响。" }, &[("arg0", entry.name.clone()), ("arg1", startup_source_text(entry.source_kind()))]);
                                this.confirm_management(
                                    label,
                                    description,
                                    false,
                                    move || {
                                        platform::set_startup_enabled(&entry, !previous)?;
                                        Ok(startup_change_message(&entry.name, previous))
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
        let summary = tf(
            "{} 个登录会话 · {} 个进程 · CPU {} · 工作集合计 {}",
            &[
                ("arg0", format!("{}", users.len())),
                (
                    "arg1",
                    format!("{}", users.iter().map(|s| s.process_count).sum::<usize>()),
                ),
                (
                    "arg2",
                    (if self.snapshot.as_ref().is_some_and(|s| s.cpu_ready) {
                        format!("{:.1}%", users.iter().map(|s| s.cpu_usage).sum::<f32>())
                    } else {
                        "—".into()
                    })
                    .to_string(),
                ),
                (
                    "arg3",
                    bytes(users.iter().map(|s| s.memory_bytes).sum()).to_string(),
                ),
            ],
        );
        v_flex().size_full().min_h_0()
            .child(h_flex().h(px(52.)).px_6().justify_between().gap_3()
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(summary))
                .child(h_flex().gap_2().children([
                    ("session-disconnect", "断开", UserSessionAction::Disconnect),
                    ("session-logoff", "注销", UserSessionAction::Logoff),
                ].into_iter().map(|(id, label, action)| {
                    Button::new(id).small().label(tr(label)).disabled(!valid)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(session) = this.selected_session(cx) {
                                let user = session.user_name.clone().unwrap_or_default();
                                let domain = session.domain.clone().unwrap_or_default();
                                let Some(logon) = session.logon_time_windows_ticks else { return; };
                                let description = tf("{label}用户 {domain}\\{user}（会话 {}）？{}", &[("label", trs(label)), ("domain", domain.to_string()), ("user", user.to_string()), ("arg0", format!("{}", session.id)), ("arg1", trs(match action { UserSessionAction::Disconnect => "应用将继续运行，远程或本地连接将断开。",
                                        UserSessionAction::Logoff => "该会话的所有应用将关闭，未保存的数据可能丢失。" }))]);
                                this.confirm_management(label, description, matches!(action, UserSessionAction::Logoff),
                                    move || { platform::control_user_session(session.id, &user, &domain, logon, action)?;
                                        Ok(session_action_message(label)) }, window, cx);
                            }
                        }))
                }))))
            .when_some(self.snapshot.as_ref().and_then(|s| s.sessions_note.clone()), |e, note| {
                e.child(div().px_6().py_2().text_xs().text_color(cx.theme().muted_foreground).child(diagnostic(&note)))
            })
            .child(div().px_6().pb_2().text_xs().text_color(cx.theme().muted_foreground)
                .child(tr("CPU 按整机为 100% 统计；工作集包含共享页，跨进程合计会重复计算共享部分；GPU 显示最繁忙进程。")))
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
                tf(
                    "{} 个应用 · 最早观察记录距今 {}",
                    &[
                        ("arg0", format!("{}", history.len())),
                        ("arg1", duration(now.saturating_sub(earliest)).to_string()),
                    ],
                )
            })
            .unwrap_or_else(|| trs("等待应用资源历史…"));
        v_flex().size_full().min_h_0()
            .child(h_flex().px_6().py_3().justify_between().child(div().text_sm().child(scope))
                .child(Button::new("clear-history").ghost().small().label(tr("清空历史…")).disabled(self.operation_busy).on_click(cx.listener(|this,_,window,cx| {
                    let commands = this.collector_commands.clone();
                    this.confirm_management("清空资源历史", trs("删除本机保存的应用资源历史，并从下一次采样重新统计？"), true, move || {
                        let (reply,receiver)=std::sync::mpsc::channel();
                        commands.try_send(CollectorCommand::ResetHistory(reply)).map_err(|e|anyhow::Error::msg(tf("采集命令未提交：{e}", &[("e", e.to_string())])))?;
                        receiver.recv_timeout(Duration::from_secs(10)).map_err(|e|anyhow::Error::msg(tf("等待清理历史失败：{e}", &[("e", e.to_string())])))?.map_err(anyhow::Error::msg)?;
                        Ok(msg("历史已清空，从下一次采样重新统计"))
                    },window,cx);
                }))))
            .child(div().px_6().pb_3().text_xs().text_color(cx.theme().muted_foreground)
                .child(tr("累计 CPU 时间与 I/O 读写从首次观察开始计算，历史保存在本机。网络累计仅计完整、无丢失的 ETW 周期；网络有效进程秒是各实例有效覆盖时长之和，缺权限/丢事件的缺口不计。I/O 包含文件、设备和其他读取/写入，不能当作应用网络流量。")))
            .when_some(self.snapshot.as_ref().and_then(|s| s.history_note.clone()), |e, note| {
                e.child(div().px_6().pb_3().text_xs().text_color(cx.theme().muted_foreground).child(diagnostic(&note)))
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
        let description = tf(
            "线程 {} · 句柄 {} · 用户 {} · 会话 {} · 私有提交 {} · CPU 时间 {}",
            &[
                ("arg0", number(p.thread_count).to_string()),
                ("arg1", number(p.handle_count).to_string()),
                ("arg2", p.user_name.as_deref().unwrap_or("—").to_string()),
                ("arg3", number(p.session_id).to_string()),
                (
                    "arg4",
                    p.private_bytes
                        .map(bytes)
                        .unwrap_or_else(|| "—".into())
                        .to_string(),
                ),
                (
                    "arg5",
                    p.cpu_time_seconds
                        .map(|seconds| format!("{seconds:.2} s"))
                        .unwrap_or_else(|| "—".into())
                        .to_string(),
                ),
            ],
        );
        v_flex().gap_2()
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(description))
            .child(h_flex().gap_2()
                .children([
                    ("process-efficiency", "效率模式…", ProcessDialog::Efficiency),
                    ("process-affinity", "CPU 亲和性…", ProcessDialog::Affinity),
                ].into_iter().map(|(id, label, kind)| {
                    let process = p.clone();
                    Button::new(id).small().ghost().label(tr(label)).disabled(!available)
                        .on_click(cx.listener(move |this, _, window, cx| this.load_process_controls(process.clone(), kind, window, cx)))
                }))
                .child(Button::new("process-end-tree").small().ghost().label(tr("结束进程树…")).disabled(!available)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let (pid, start) = (p.pid, p.start_time_windows_ticks.unwrap_or(0));
                        this.confirm_management("结束进程树", tf("结束 {}（PID {pid}）及其当前子进程？所有目标都会重新核验身份。未保存的数据可能丢失。", &[("arg0", p.name.to_string()), ("pid", format!("{pid}"))]),
                            true, move || {
                                let ended = platform::terminate_process_tree(pid, start)?;
                                Ok(mf("已结束 {} 个进程：{ended:?}", &[("arg0", format!("{}", ended.len())), ("ended", format!("{ended:?}"))]))
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
        self.message = Some(msg("正在读取进程设置…"));
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
                                        tf("{}（PID {}）当前效率模式{}。{}", &[("arg0", process.name.to_string()), ("arg1", format!("{}", process.pid)), ("arg2", trs(if controls.efficiency_mode { "已启用" } else { "未启用" })), ("arg3", trs(if enabled { "启用后将降低调度优先级并使用 EcoQoS，后台任务可能更慢。" }
                                                else { "关闭后取消 EcoQoS 限制，并恢复空闲优先级为正常。" }))]),
                                        false, move || { platform::set_efficiency_mode(pid, start, enabled)?;
                                            Ok(efficiency_change_message(enabled)) }, window, cx);
                                }
                            }
                        }
                        Err(error) => this.message = Some(mf("进程设置不可用：{error:#}", &[("error", format!("{error:#}"))])),
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
                .placeholder(tr("例如 0x3"))
        });
        let error = cx.new(|_| String::new());
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog.title(tr("CPU 亲和性")).width(px(520.))
                .child(v_flex().gap_3()
                    .child(tf("{} · PID {} · 处理器组 {}", &[("arg0", process.name.to_string()), ("arg1", format!("{}", process.pid)), ("arg2", format!("{}", controls.processor_group))]))
                    .child(div().text_sm().child(tf("可用掩码 0x{:X}；当前掩码 0x{:X}", &[("arg0", format!("{:X}", controls.system_affinity_mask)), ("arg1", format!("{:X}", controls.affinity_mask))])))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground)
                        .child(tr("使用十六进制掩码：每一位对应该处理器组的一个逻辑 CPU，1 表示允许使用。例如 0x3 选择 CPU 0 和 1。")))
                    .child(Input::new(&input))
                    .child(div().text_sm().text_color(rgb(0xf87171)).child(error.read(cx).clone())))
                .button_props(DialogButtonProps::default().ok_text(tr("应用亲和性")).cancel_text(tr("取消")).show_cancel(true))
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
                                Ok(mf("CPU 亲和性已设为 0x{mask:X}", &[("mask", format!("{mask:X}"))]))
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
    use super::super::localization::trs_in;
    use super::{
        affinity_mask, efficiency_change_message, matching_service, matching_session,
        session_action_message, startup_change_message, startup_row_matches, unique_matching,
    };
    use system_monitor_x::{
        i18n::Language,
        platform::{ServiceInfo, ServiceState},
        sessions::SessionSample,
    };

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
            state: ServiceState::Running,
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

    #[test]
    fn cached_startup_results_relocalize_without_translating_application_identity() {
        let raw_name = "运行中 / 日本語 %{arg0}.exe";
        let enabled = startup_change_message(raw_name, false);
        let disabled = startup_change_message(raw_name, true);
        for language in Language::SUPPORTED {
            let enabled_text = enabled.render_in(language);
            let disabled_text = disabled.render_in(language);
            assert!(
                enabled_text.contains(raw_name),
                "{language:?}: {enabled_text}"
            );
            assert!(
                disabled_text.contains(raw_name),
                "{language:?}: {disabled_text}"
            );
            assert_ne!(enabled_text, disabled_text, "{language:?}");
            assert!(!enabled_text.replace(raw_name, "").contains("%{"));
            assert!(!disabled_text.replace(raw_name, "").contains("%{"));
        }
        assert_ne!(
            enabled.render_in(Language::ZhCn),
            enabled.render_in(Language::EnUs)
        );
        assert_ne!(
            disabled.render_in(Language::ZhCn),
            disabled.render_in(Language::EnUs)
        );
    }

    #[test]
    fn cached_session_action_and_efficiency_results_keep_owned_labels_localizable() {
        for source in ["断开", "注销"] {
            let message = session_action_message(source);
            for language in Language::SUPPORTED {
                let rendered = message.render_in(language);
                assert!(rendered.contains(trs_in(language, source).as_str()));
                assert!(!rendered.contains("%{"));
            }
            assert_ne!(
                message.render_in(Language::ZhCn),
                message.render_in(Language::EnUs)
            );
        }
        let enabled = efficiency_change_message(true);
        let disabled = efficiency_change_message(false);
        for language in Language::SUPPORTED {
            assert_ne!(enabled.render_in(language), disabled.render_in(language));
        }
        assert_ne!(
            enabled.render_in(Language::ZhCn),
            enabled.render_in(Language::EnUs)
        );
    }

    #[test]
    fn startup_selection_uses_raw_key_when_localized_sources_collide() {
        let entries = [
            ("same.exe", "same command", "same source", "user-key"),
            ("same.exe", "same command", "same source", "machine-key"),
        ];
        for translated in [
            "当前用户启动文件夹",
            "Current user startup folder",
            "スタートアップ フォルダー",
            "시작 폴더",
            "Autostartordner",
            "Dossier de démarrage",
            "Carpeta de inicio",
            "Pasta de inicialização",
        ] {
            let row = vec![
                "same.exe".into(),
                "localized enabled status".into(),
                "same command".into(),
                translated.into(),
                "machine-key".into(),
            ];
            let selected = unique_matching(&entries, |entry| {
                startup_row_matches(entry.0, entry.1, entry.2, || entry.3.into(), &row)
            })
            .unwrap();
            assert_eq!(selected.3, "machine-key");
        }
    }

    #[test]
    fn stale_startup_keys_and_ambiguous_legacy_rows_never_select_another_origin() {
        let entries = [
            ("same.exe", "same command", "same source", "user-key"),
            ("same.exe", "same command", "same source", "machine-key"),
        ];
        let mut row = vec![
            "same.exe".into(),
            "enabled".into(),
            "same command".into(),
            "same source".into(),
        ];
        let select = |row: &[String]| {
            unique_matching(&entries, |entry| {
                startup_row_matches(entry.0, entry.1, entry.2, || entry.3.into(), row)
            })
        };
        assert!(select(&row).is_none()); // Display columns cannot distinguish origins.
        row.push("retired-identity".into());
        assert!(select(&row).is_none());
        row[4].clear();
        assert!(select(&row).is_none()); // No fallback for a malformed key.
        row[4] = "user-key".into();
        assert_eq!(select(&row).unwrap().3, "user-key");
        row[2] = "replacement command".into();
        assert!(select(&row).is_none());
        let legacy = vec![
            "same.exe".into(),
            "enabled".into(),
            "same command".into(),
            "same source".into(),
        ];
        assert!(startup_row_matches(
            entries[0].0,
            entries[0].1,
            entries[0].2,
            || panic!("Legacy selection must not calculate a key"),
            &legacy,
        ));
    }
}
