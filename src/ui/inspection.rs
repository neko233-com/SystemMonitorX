use super::*;
use anyhow::{Context as _, ensure};
use system_monitor_x::process_diagnostics::{
    DiagnosticIssueCode, EnumerationStatus, MemoryKind, MemoryState, PageAccess, ProcessDiagnostics,
};

const INSPECTION_VISIBLE_ROWS: usize = 256;

fn enumeration_label(status: EnumerationStatus) -> SharedString {
    tr(match status {
        EnumerationStatus::Complete => "已完成",
        EnumerationStatus::Partial => "部分数据",
        EnumerationStatus::Unavailable => "不可用",
    })
}
fn memory_state_label(state: MemoryState) -> SharedString {
    tr(match state {
        MemoryState::Committed => "已提交",
        MemoryState::Reserved => "保留",
        MemoryState::Free => "未分配",
        MemoryState::Unknown => "未知",
    })
}
fn memory_kind_label(kind: MemoryKind) -> SharedString {
    tr(match kind {
        MemoryKind::Private => "私有",
        MemoryKind::Mapped => "映射",
        MemoryKind::Image => "映像",
        MemoryKind::Unknown => "未知",
    })
}
fn page_access_label(access: PageAccess) -> String {
    trs(match access {
        PageAccess::Allowed => "允许",
        PageAccess::Denied => "不允许",
        PageAccess::Guarded => "防护页",
        PageAccess::Unknown => "未知",
        PageAccess::NotApplicable => "不适用",
    })
}
fn diagnostic_issue_label(code: DiagnosticIssueCode) -> SharedString {
    tr(match code {
        DiagnosticIssueCode::ModuleSnapshotScope => "模块列表不包含仅作为数据文件加载的映像。",
        DiagnosticIssueCode::Wow64ModuleSnapshot => "已同时请求 32 位和 64 位模块快照。",
        DiagnosticIssueCode::ArchitectureUnavailable => "无法读取进程架构。",
        DiagnosticIssueCode::ModuleSnapshotFailed => "无法读取模块快照。",
        DiagnosticIssueCode::ModuleEnumerationFailed => "模块读取中断，当前列表可能不完整。",
        DiagnosticIssueCode::ModuleSnapshotChanging => "模块列表正在变化，请稍后刷新。",
        DiagnosticIssueCode::ModuleLimit => "模块数量达到读取上限，当前列表为部分数据。",
        DiagnosticIssueCode::MemoryRegionLimit => "内存区域达到读取上限，汇总只包含已读取部分。",
        DiagnosticIssueCode::TimeLimit => "读取达到时间上限，当前结果可能不完整。",
        DiagnosticIssueCode::CrossBitnessUnavailable => "进程架构或系统读取限制使此数据不可用。",
        DiagnosticIssueCode::ModuleTextMayBeTruncated => {
            "部分模块路径可能被截短；JSON 保留原始字符。"
        }
        DiagnosticIssueCode::UnexpectedModuleIdentity => "模块身份不匹配，已停止读取。",
        DiagnosticIssueCode::MemoryAccessUnavailable => "无法读取虚拟内存信息，访问权限可能不足。",
        DiagnosticIssueCode::MemoryQueryFailed => "内存区域读取失败，汇总可能不完整。",
        DiagnosticIssueCode::InvalidMemoryRegion => "系统返回的内存区域无效，已停止读取。",
        DiagnosticIssueCode::SummaryOverflow => "内存统计超出范围，已停止汇总。",
        DiagnosticIssueCode::MemorySummaryScope => {
            "内存区域按顺序读取；进程仍在运行，结果不是冻结快照。"
        }
    })
}

impl SystemMonitorApp {
    pub(super) fn open_process_tools(
        &mut self,
        pid: u32,
        ticks: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ticks == 0 {
            return;
        }
        let Some(process) = self
            .snapshot
            .as_ref()
            .and_then(|s| {
                s.processes
                    .iter()
                    .find(|p| p.pid == pid && p.start_time_windows_ticks == Some(ticks))
            })
            .cloned()
        else {
            self.message = Some(msg("进程信息已过期，请刷新后重试"));
            cx.notify();
            return;
        };
        self.inspection_identity = Some((pid, ticks));
        self.inspection = None;
        self.inspection_error = None;
        self.inspection_loading = false;
        self.inspection_show_memory = false;
        self.refresh_inspection(pid, ticks, cx);
        let parent = cx.entity().downgrade();
        let refresh =
            std::rc::Rc::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.refresh_inspection(pid, ticks, cx)
            }));
        let copy_report = std::rc::Rc::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.export_diagnostic(pid, ticks, false, cx)
        }));
        let save_report = std::rc::Rc::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.export_diagnostic(pid, ticks, true, cx)
        }));
        let copy_process = std::rc::Rc::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.copy_process_query(pid, ticks, false, cx)
        }));
        let copy_ports = std::rc::Rc::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.copy_process_query(pid, ticks, true, cx)
        }));
        let show_modules = std::rc::Rc::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if this.inspection_identity == Some((pid, ticks)) {
                this.inspection_show_memory = false;
                cx.notify();
            }
        }));
        let show_memory = std::rc::Rc::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if this.inspection_identity == Some((pid, ticks)) {
                this.inspection_show_memory = true;
                cx.notify();
            }
        }));
        let executable = process.exe.clone();
        let terminal = std::rc::Rc::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if let Some(path) = executable.clone() {
                this.open_program_terminal(path, cx);
            }
        }));
        let process_name = process.name;
        let has_path = process.exe.is_some();
        window.open_dialog(cx, move |dialog, _, cx| {
            let view = parent.upgrade();
            let app = view.as_ref().map(|view| view.read(cx));
            let matching = app.is_some_and(|app| app.inspection_identity == Some((pid, ticks)));
            let report = app.filter(|_| matching).and_then(|app| app.inspection.clone());
            let loading = app.is_some_and(|app| matching && app.inspection_loading);
            let show_memory_tab = app.is_some_and(|app| matching && app.inspection_show_memory);
            let error = app.filter(|_| matching).and_then(|app| app.inspection_error.clone());
            let mut content = v_flex().id("process-tools-content").test_support()
                .max_h(px(390.)).overflow_y_scroll().gap_4()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{process_name} · PID {pid}")))
                .child(h_flex().flex_wrap().gap_2()
                    .child(Button::new("copy-process-query").small().ghost().label(tr("复制 PowerShell 查询"))
                        .on_click({ let run = copy_process.clone(); move |e,w,cx| run(e,w,cx) }))
                    .child(Button::new("copy-port-query").small().ghost().label(tr("复制端口查询"))
                        .on_click({ let run = copy_ports.clone(); move |e,w,cx| run(e,w,cx) }))
                    .child(Button::new("open-process-terminal").small().ghost().label(tr("在程序目录打开 PowerShell")).disabled(!has_path)
                        .on_click({ let run = terminal.clone(); move |e,w,cx| run(e,w,cx) })))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child(tr("导出不含命令行或环境变量；包含进程路径与网络地址。")))
                .when(loading, |e| e.child(tr("正在读取进程诊断…")))
                .when_some(error, |e,error| e.child(div().text_color(cx.theme().muted_foreground).child(error.render())));
            if let Some(report) = report {
                content = content
                    .child(h_flex().flex_wrap().gap_3().child(tr("模块快照")).child(enumeration_label(report.modules.status))
                        .child(tr("虚拟地址空间")).child(enumeration_label(report.memory.status)))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground)
                        .child(tr("只读采样；虚拟地址空间不等于实际驻留内存。")));
                if report.memory.status != EnumerationStatus::Unavailable {
                    let memory = &report.memory.summary;
                    content = content.child(v_flex().gap_1()
                        .child(Self::detail_row(tr("已提交"), bytes(memory.committed_bytes), cx))
                        .child(Self::detail_row(tr("保留"), bytes(memory.reserved_bytes), cx))
                        .child(Self::detail_row(tr("私有提交"), bytes(memory.private_committed_bytes), cx))
                        .child(Self::detail_row(tr("可执行区域"), bytes(memory.executable_committed_bytes), cx)));
                }
                content = content.child(h_flex().gap_2().flex_wrap()
                    .child(Button::new("inspection-modules-tab").small().label(tr("模块"))
                        .when(!show_memory_tab, |button| button.primary())
                        .when(show_memory_tab, |button| button.ghost()).on_click({ let run = show_modules.clone(); move |e,w,cx| run(e,w,cx) }))
                    .child(Button::new("inspection-memory-tab").small().label(tr("内存区域"))
                        .when(show_memory_tab, |button| button.primary())
                        .when(!show_memory_tab, |button| button.ghost()).on_click({ let run = show_memory.clone(); move |e,w,cx| run(e,w,cx) })));
                if show_memory_tab {
                    let count = report.memory.regions.len().min(INSPECTION_VISIBLE_ROWS);
                    content = content
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(tr("内存区域")))
                        .child(div().text_xs().text_color(cx.theme().muted_foreground)
                            .child(tr("页面权限来自系统元数据，未实际读取或执行目标内存。")))
                        .when(report.memory.regions.is_empty(), |e| e.child(tr("没有可显示的数据")))
                        .child(v_flex().id("memory-region-list").test_support().gap_2().children(
                            report.memory.regions.iter().take(INSPECTION_VISIBLE_ROWS).map(|region| {
                                v_flex().pb_2().border_b_1().border_color(cx.theme().border).gap_1()
                                    .child(div().font_weight(FontWeight::SEMIBOLD)
                                        .child(format!("0x{:016X} · {}", region.base_address, bytes(region.size_bytes))))
                                    .child(h_flex().flex_wrap().gap_2().child(memory_state_label(region.state))
                                        .when_some(region.kind, |e,kind| e.child(memory_kind_label(kind))))
                                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(tf(
                                        "读取：{read} · 写入：{write} · 执行：{execute}",
                                        &[("read",page_access_label(region.protection.readable)),
                                          ("write",page_access_label(region.protection.writable)),
                                          ("execute",page_access_label(region.protection.executable))])))
                            })))
                        .when(report.memory.regions.len() > count, |e| e.child(div().text_xs().text_color(cx.theme().muted_foreground).child(tf(
                            "显示前 {count} 条，共 {total} 条；全部已读数据请导出 JSON。",
                            &[("count",count.to_string()),("total",report.memory.regions.len().to_string())]))));
                } else {
                    let count = report.modules.entries.len().min(INSPECTION_VISIBLE_ROWS);
                    content = content.child(div().font_weight(FontWeight::SEMIBOLD).child(tr("DLL 与已加载模块")))
                        .when(report.modules.entries.is_empty(), |e| e.child(tr("没有可显示的数据")))
                        .child(v_flex().id("module-list").test_support().gap_2().children(
                            report.modules.entries.iter().take(INSPECTION_VISIBLE_ROWS).map(|module| {
                                v_flex().pb_2().border_b_1().border_color(cx.theme().border).gap_1()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                                        "{} · {} · 0x{:X}", module.name, bytes(module.image_size_bytes), module.base_address)))
                                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(module.path.clone()))
                            })))
                        .when(report.modules.entries.len() > count, |e| e.child(div().text_xs().text_color(cx.theme().muted_foreground).child(tf(
                            "显示前 {count} 条，共 {total} 条；全部已读数据请导出 JSON。",
                            &[("count",count.to_string()),("total",report.modules.entries.len().to_string())]))));
                }
                let issues: Vec<_> = report.issues.iter().chain(report.modules.issues.iter())
                    .chain(report.memory.issues.iter()).collect();
                if !issues.is_empty() {
                    content = content.child(v_flex().gap_2()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(tr("诊断说明")))
                        .children(issues.iter().map(|issue| div().text_xs().text_color(cx.theme().muted_foreground)
                            .child(diagnostic_issue_label(issue.code)))));
                    let raw: Vec<_> = issues.iter().copied().filter(|issue|
                        issue.win32_error.is_some() || issue.code == DiagnosticIssueCode::InvalidMemoryRegion
                    ).collect();
                    if !raw.is_empty() {
                        content = content.child(v_flex().gap_1()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(tr("原始 API 详情")))
                            .children(raw.into_iter().map(|issue| div().text_xs().text_color(cx.theme().muted_foreground)
                                .child(issue.diagnostic.clone()))));
                    }
                }
            }
            dialog.title(tr("进程工具")).width(px(720.)).child(content).footer(
                h_flex().flex_wrap().gap_2().justify_end()
                    .child(Button::new("refresh-process-tools").small().label(tr("刷新列表")).disabled(loading)
                        .on_click({let run=refresh.clone();move |e,w,cx|run(e,w,cx)}))
                    .child(Button::new("copy-diagnostic-json").small().label(tr("复制诊断 JSON")).disabled(!matching||loading)
                        .on_click({let run=copy_report.clone();move |e,w,cx|run(e,w,cx)}))
                    .child(Button::new("save-diagnostic-json").small().label(tr("保存诊断 JSON")).disabled(!matching||loading)
                        .on_click({let run=save_report.clone();move |e,w,cx|run(e,w,cx)}))
                    .child(Button::new("process-tools-close").small().label(tr("关闭")).on_click(|_,w,cx|w.close_dialog(cx))))
        });
    }
    fn refresh_inspection(&mut self, pid: u32, ticks: u64, cx: &mut Context<Self>) {
        if self.inspection_loading && self.inspection_identity == Some((pid, ticks)) {
            return;
        }
        self.inspection_identity = Some((pid, ticks));
        self.inspection_loading = true;
        self.inspection_error = None;
        self.inspection_generation = self.inspection_generation.wrapping_add(1);
        let generation = self.inspection_generation;
        cx.spawn(async move |view, cx| {
            let result =
                smol::unblock(move || ProcessDiagnostics::read(pid, ticks).map(Arc::new)).await;
            let _ = view.update(cx, |this, cx| {
                if this.inspection_identity != Some((pid, ticks))
                    || this.inspection_generation != generation
                {
                    return;
                }
                this.inspection_loading = false;
                match result {
                    Ok(report) => this.inspection = Some(report),
                    Err(error) => {
                        this.inspection = None;
                        this.inspection_error = Some(mf(
                            "诊断读取失败：{error}",
                            &[("error", format!("{error:#}"))],
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn copy_process_query(&mut self, pid: u32, ticks: u64, ports: bool, cx: &mut Context<Self>) {
        use system_monitor_x::developer_tools::{
            ProcessCommandKind, ProcessIdentity, process_command,
        };
        let identity = ProcessIdentity {
            pid,
            creation_ticks: Some(ticks),
        };
        let result: anyhow::Result<String> = if ports {
            (|| {
                Ok(format!(
                    "{}\n\n{}",
                    process_command(identity, ProcessCommandKind::PowerShellTcp)?.text,
                    process_command(identity, ProcessCommandKind::PowerShellUdp)?.text
                ))
            })()
        } else {
            process_command(identity, ProcessCommandKind::PowerShellProcess)
                .map(|command| command.text)
        };
        match result {
            Ok(command) => self.copy_developer_text(command, msg("查询命令"), cx),
            Err(error) => {
                self.message = Some(error.to_string().into());
                cx.notify();
            }
        }
    }
    fn open_program_terminal(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.run_operation(
            "正在打开 PowerShell",
            move || {
                use windows::Win32::UI::Shell::ShellExecuteW;
                use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
                use windows::core::PCWSTR;
                let folder = path.parent().context("Executable folder is unavailable")?;
                ensure!(folder.is_dir(), "Executable folder is unavailable");
                let literal = system_monitor_x::developer_tools::quote_powershell_literal(
                    &folder.to_string_lossy(),
                )?;
                use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
                let mut system = [0_u16; 32_768];
                // SAFETY: bounded writable buffer; use the installed system
                // directory API rather than an inherited environment variable.
                let length = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
                ensure!(
                    length > 0 && length < system.len(),
                    "Windows system directory is unavailable"
                );
                let directory = String::from_utf16(&system[..length])
                    .context("Windows system directory has invalid UTF-16")?;
                let executable =
                    PathBuf::from(directory).join("WindowsPowerShell/v1.0/powershell.exe");
                ensure!(executable.is_file(), "Windows PowerShell is unavailable");
                let command =
                    format!("-NoProfile -NoExit -Command \"Set-Location -LiteralPath {literal}\"");
                let exe: Vec<u16> = executable
                    .as_os_str()
                    .to_string_lossy()
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                let args: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
                let result = unsafe {
                    ShellExecuteW(
                        None,
                        PCWSTR::null(),
                        PCWSTR(exe.as_ptr()),
                        PCWSTR(args.as_ptr()),
                        PCWSTR::null(),
                        SW_SHOWNORMAL,
                    )
                };
                ensure!(result.0 as isize > 32, "Windows PowerShell launch failed");
                Ok(msg("已打开程序目录的 PowerShell"))
            },
            cx,
        );
    }
    fn export_diagnostic(&mut self, pid: u32, ticks: u64, save: bool, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let Some(index) = snapshot
            .processes
            .iter()
            .position(|p| p.pid == pid && p.start_time_windows_ticks == Some(ticks))
        else {
            self.message = Some(msg("进程信息已过期，请刷新后重试"));
            cx.notify();
            return;
        };
        let ports = self.ports_snapshot.clone();
        let details = self
            .process_details
            .as_ref()
            .filter(|d| d.pid == pid && d.creation_ticks == ticks)
            .map(|d| (**d).clone());
        let inspection = self
            .inspection
            .as_ref()
            .filter(|d| d.pid == pid && d.creation_ticks == ticks)
            .cloned();
        let prompt = save.then(|| {
            cx.prompt_for_new_path(
                &std::env::var_os("USERPROFILE")
                    .map(PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir),
                Some(&format!("SystemMonitorX-PID-{pid}.json")),
            )
        });
        cx.spawn(async move |view, cx| {
            let result = smol::unblock(move || -> anyhow::Result<String> {
                use system_monitor_x::developer_tools::{
                    DiagnosticCapture, ExportOptions, ProcessIdentity,
                };
                let mut capture = DiagnosticCapture::for_process(snapshot, index, ports, details)?;
                if let Some(inspection) = inspection {
                    capture = capture.with_inspection(
                        ProcessIdentity {
                            pid,
                            creation_ticks: Some(ticks),
                        },
                        "Win32 Toolhelp32 / VirtualQueryEx",
                        serde_json::to_value(&*inspection)?,
                    )?;
                }
                capture.to_json_pretty(ExportOptions::default())
            })
            .await;
            match result {
                Ok(json) => {
                    if let Some(prompt) = prompt {
                        if let Ok(Ok(Some(path))) = prompt.await {
                            let result = smol::unblock(move || {
                                std::fs::write(&path, json)?;
                                Ok::<_, anyhow::Error>(path)
                            })
                            .await;
                            let _ = view.update(cx, |this, cx| {
                                this.message = Some(match result {
                                    Ok(path) => mf(
                                        "进程诊断已保存：{path}",
                                        &[("path", path.to_string_lossy().into_owned())],
                                    ),
                                    Err(error) => mf(
                                        "诊断读取失败：{error}",
                                        &[("error", format!("{error:#}"))],
                                    ),
                                });
                                cx.notify();
                            });
                        }
                    } else {
                        let _ = view.update(cx, |this, cx| {
                            this.copy_developer_text(json, msg("进程诊断 JSON"), cx)
                        });
                    }
                }
                Err(error) => {
                    let _ = view.update(cx, |this, cx| {
                        this.message = Some(mf(
                            "诊断读取失败：{error}",
                            &[("error", format!("{error:#}"))],
                        ));
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }
}
