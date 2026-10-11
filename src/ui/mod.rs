#[cfg(feature = "ui-test")]
mod qa;
mod tables;

use gpui_kit::assets::IconName as UiIcon;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::*,
    chart::AreaChart,
    dialog::DialogButtonProps,
    input::{Input, InputEvent, InputState},
    table::*,
    *,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use system_monitor_x::{
    export::write_process_csv,
    monitor::{Monitor, SystemSnapshot},
    platform::{self, Priority},
};
use tables::{ProcessTable, TextTable, bytes, duration, percent, rate};

gpui_kit::actions!(
    system_monitor_x,
    [FocusSearch, TogglePause, NewTask, ExportCsv]
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Processes,
    Performance,
    Devices,
    Services,
    Startup,
    Settings,
}
impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Processes => "进程",
            Self::Performance => "性能",
            Self::Devices => "设备与传感器",
            Self::Services => "服务",
            Self::Startup => "启动应用",
            Self::Settings => "设置",
        }
    }
    fn icon(self) -> UiIcon {
        match self {
            Self::Processes => UiIcon::Layers,
            Self::Performance => UiIcon::Activity,
            Self::Devices => UiIcon::Cpu,
            Self::Services => UiIcon::Settings2,
            Self::Startup => UiIcon::Rocket,
            Self::Settings => UiIcon::Settings,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    interval_ms: u64,
    dark: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            dark: true,
        }
    }
}
impl Settings {
    fn path() -> PathBuf {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into()))
            .join("SystemMonitorX/settings.json")
    }
    fn load() -> Self {
        let mut s: Self = std::fs::read(Self::path())
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default();
        if ![500, 1000, 2000].contains(&s.interval_ms) {
            s.interval_ms = 1000
        }
        s
    }
    fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}
#[derive(Clone)]
struct HistoryPoint {
    time: String,
    cpu: f64,
    memory: f64,
    gpu: Option<f64>,
    disk: Option<f64>,
    net_rx: Option<f64>,
    net_tx: Option<f64>,
}
type SnapshotSlot = Arc<Mutex<Option<(u64, Arc<SystemSnapshot>)>>>;

pub struct SystemMonitorApp {
    page: Page,
    snapshot: Option<Arc<SystemSnapshot>>,
    slot: SnapshotSlot,
    last_seq: u64,
    stopped: Arc<AtomicBool>,
    interval: Arc<AtomicU64>,
    paused: bool,
    settings: Settings,
    search: Entity<InputState>,
    task_input: Entity<InputState>,
    processes: Entity<TableState<ProcessTable>>,
    services: Entity<TableState<TextTable>>,
    startup: Entity<TableState<TextTable>>,
    history: VecDeque<HistoryPoint>,
    sample_index: u64,
    message: Option<String>,
    elevated: bool,
    metadata_loading: bool,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}
impl Drop for SystemMonitorApp {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}
impl SystemMonitorApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Theme::change(
            if settings.dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            Some(window),
            cx,
        );
        let slot: SnapshotSlot = Arc::new(Mutex::new(None));
        let stopped = Arc::new(AtomicBool::new(false));
        let interval = Arc::new(AtomicU64::new(settings.interval_ms));
        let (worker_slot, worker_stop, worker_interval) =
            (slot.clone(), stopped.clone(), interval.clone());
        std::thread::Builder::new()
            .name("system-collector".into())
            .spawn(move || {
                let mut monitor = Monitor::new();
                let mut seq = 0;
                while !worker_stop.load(Ordering::Relaxed) {
                    let sample = Arc::new(monitor.sample());
                    seq += 1;
                    if let Ok(mut slot) = worker_slot.lock() {
                        *slot = Some((seq, sample));
                    }
                    let mut elapsed = 0;
                    while elapsed < worker_interval.load(Ordering::Relaxed)
                        && !worker_stop.load(Ordering::Relaxed)
                    {
                        std::thread::sleep(Duration::from_millis(100));
                        elapsed += 100;
                    }
                }
            })
            .expect("start collector");
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索进程名称、路径或 PID"));
        let task_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("例如 notepad.exe 或可执行文件路径"));
        let processes = cx.new(|cx| {
            TableState::new(ProcessTable::new(), window, cx)
                .col_selectable(false)
                .col_movable(false)
        });
        let services = cx.new(|cx| {
            TableState::new(
                TextTable::new(&[
                    ("服务名称", 260.),
                    ("显示名称", 360.),
                    ("状态", 120.),
                    ("PID", 80.),
                    ("启动类型", 130.),
                ]),
                window,
                cx,
            )
            .col_selectable(false)
            .col_movable(false)
        });
        let startup = cx.new(|cx| {
            TableState::new(
                TextTable::new(&[("名称", 260.), ("命令 / 路径", 640.), ("来源", 280.)]),
                window,
                cx,
            )
            .col_selectable(false)
            .col_movable(false)
        });
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filter(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&processes, |_, table, event: &TableEvent, cx| {
                match event {
                    TableEvent::SelectRow(i) | TableEvent::DoubleClickedRow(i) => {
                        let id = table
                            .read(cx)
                            .delegate()
                            .rows
                            .get(*i)
                            .map(|p| (p.pid, p.start_time_unix_secs));
                        table.update(cx, |t, _| t.delegate_mut().selected = id);
                    }
                    TableEvent::ClearSelection => {
                        table.update(cx, |t, _| t.delegate_mut().selected = None)
                    }
                    _ => {}
                }
                cx.notify();
            }),
        ];
        let poll = cx.spawn(async move |view, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(100)).await;
                if view.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        });
        let mut app = Self {
            page: Page::Processes,
            snapshot: None,
            slot,
            last_seq: 0,
            stopped,
            interval,
            paused: false,
            settings,
            search,
            task_input,
            processes,
            services,
            startup,
            history: VecDeque::new(),
            sample_index: 0,
            message: None,
            elevated: platform::is_elevated().unwrap_or(false),
            metadata_loading: false,
            focus_handle,
            _subscriptions: subscriptions,
            _poll: poll,
        };
        app.refresh_metadata(cx);
        app
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        if self.paused {
            return;
        }
        let next = self.slot.lock().ok().and_then(|s| s.clone());
        let Some((seq, snapshot)) = next else {
            return;
        };
        if seq == self.last_seq {
            return;
        }
        self.last_seq = seq;
        self.sample_index += 1;
        if snapshot.cpu_ready {
            self.history.push_back(HistoryPoint {
                time: format!("{}", self.sample_index),
                cpu: snapshot.cpu_usage as f64,
                memory: snapshot.memory_percent() as f64,
                gpu: snapshot.gpu_percent().map(|n| n as f64),
                disk: snapshot.disk_usage.map(|n| n as f64),
                net_rx: snapshot.network_receive_bytes_per_sec,
                net_tx: snapshot.network_transmit_bytes_per_sec,
            });
            if self.history.len() > 120 {
                self.history.pop_front();
            }
        }
        self.processes.update(cx, |table, cx| {
            let d = table.delegate_mut();
            d.all = snapshot.processes.clone();
            d.ready = snapshot.cpu_ready;
            d.memory_total = snapshot.memory_total;
            d.rebuild();
            tables::sync_selection(table, cx);
            cx.notify();
        });
        self.snapshot = Some(snapshot);
        cx.notify();
    }
    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_string();
        self.processes.update(cx, |t, cx| {
            t.delegate_mut().query = query.clone();
            t.delegate_mut().rebuild();
            tables::sync_selection(t, cx);
            cx.notify();
        });
        for table in [&self.services, &self.startup] {
            table.update(cx, |t, cx| {
                t.delegate_mut().query = query.clone();
                t.delegate_mut().rebuild();
                cx.notify();
            });
        }
    }
    fn set_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        // Programmatic InputState::set_value does not emit InputEvent::Change.
        // Rebuild all tables explicitly so a query does not leak across pages.
        self.filter(cx);
        cx.notify();
    }
    fn refresh_metadata(&mut self, cx: &mut Context<Self>) {
        if self.metadata_loading {
            return;
        }
        self.metadata_loading = true;
        cx.spawn(async move |view, cx| {
            let results =
                smol::unblock(|| (platform::list_services(), platform::list_startup())).await;
            let _ = view.update(cx, |this, cx| {
                this.metadata_loading = false;
                match results.0 {
                    Ok(rows) => this.services.update(cx, |t, cx| {
                        t.delegate_mut().all = rows
                            .into_iter()
                            .map(|s| {
                                vec![
                                    s.name,
                                    s.display_name,
                                    s.status,
                                    s.pid.to_string(),
                                    s.start_type,
                                ]
                            })
                            .collect();
                        t.delegate_mut().rebuild();
                        cx.notify();
                    }),
                    Err(e) => this.message = Some(format!("服务读取失败：{e:#}")),
                }
                match results.1 {
                    Ok(rows) => this.startup.update(cx, |t, cx| {
                        t.delegate_mut().all = rows
                            .into_iter()
                            .map(|s| vec![s.name, s.command, s.source])
                            .collect();
                        t.delegate_mut().rebuild();
                        cx.notify();
                    }),
                    Err(e) => this.message = Some(format!("启动项读取失败：{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn toggle_pause(&mut self, _: &TogglePause, _: &mut Window, cx: &mut Context<Self>) {
        self.paused = !self.paused;
        cx.notify();
    }
    fn focus_search(&mut self, _: &FocusSearch, w: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(w, cx));
    }
    fn new_task(&mut self, _: &NewTask, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.task_input.clone();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("运行新任务")
                .width(px(480.))
                .child(Input::new(&input))
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("task-cancel")
                                .label("取消")
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(Button::new("task-run").primary().label("运行").on_click({
                            let input = input.clone();
                            let view = view.clone();
                            move |_, w, cx| {
                                if Self::submit_task(&input, &view, cx) {
                                    w.close_dialog(cx);
                                }
                            }
                        })),
                )
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("运行")
                        .cancel_text("取消")
                        .show_cancel(true),
                )
                .on_ok({
                    let input = input.clone();
                    let view = view.clone();
                    move |_, _, cx| Self::submit_task(&input, &view, cx)
                })
        });
    }
    fn submit_task(input: &Entity<InputState>, view: &WeakEntity<Self>, cx: &mut App) -> bool {
        let command = input.read(cx).value().to_string();
        let result = platform::launch_task(&command);
        let success = result.is_ok();
        let _ = view.update(cx, |this, cx| {
            this.message = Some(match result {
                Ok(()) => "任务已启动".into(),
                Err(e) => format!("启动失败：{e:#}"),
            });
            cx.notify();
        });
        success
    }
    fn end_task(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.processes.read(cx).delegate().selection().cloned() else {
            return;
        };
        let view = cx.entity().downgrade();
        w.open_alert_dialog(cx, move |dialog, _, _| {
            dialog
                .confirm()
                .title("结束进程")
                .description(format!(
                    "结束 {}（PID {}）？未保存的数据可能丢失。",
                    p.name, p.pid
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("结束进程")
                        .cancel_text("取消")
                        .show_cancel(true)
                        .ok_variant(ButtonVariant::Danger),
                )
                .on_ok({
                    let view = view.clone();
                    move |_, _, cx| {
                        let result = platform::terminate_process(p.pid, p.start_time_unix_secs);
                        let _ = view.update(cx, |this, cx| {
                            this.message = Some(match result {
                                Ok(()) => "进程已结束".into(),
                                Err(e) => format!("无法结束进程：{e:#}"),
                            });
                            cx.notify();
                        });
                        true
                    }
                })
        });
    }
    fn export_csv(&mut self, _: &ExportCsv, _: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let directory =
            PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_else(|| ".".into()));
        let prompt = cx.prompt_for_new_path(&directory, Some("SystemMonitorX-processes.csv"));
        cx.spawn(async move |view, cx| {
            if let Ok(Ok(Some(path))) = prompt.await {
                let result =
                    smol::unblock(move || write_process_csv(&snapshot, &path).map(|_| path)).await;
                let _ = view.update(cx, |this, cx| {
                    this.message = Some(match result {
                        Ok(path) => format!("已导出 {}", path.display()),
                        Err(e) => format!("导出失败：{e:#}"),
                    });
                    cx.notify();
                });
            }
        })
        .detach();
    }
    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w(px(210.))
            .flex_shrink_0()
            .h_full()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .h(px(76.))
                    .px_5()
                    .gap_3()
                    .child(
                        Icon::new(UiIcon::Activity)
                            .size(px(25.))
                            .text_color(rgb(0x63aaf9)),
                    )
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_base()
                                    .child("SystemMonitorX"),
                            )
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child("WINDOWS SYSTEM MONITOR"),
                            ),
                    ),
            )
            .child(
                div()
                    .px_5()
                    .pt_4()
                    .pb_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("系统"),
            )
            .children(
                [
                    Page::Processes,
                    Page::Performance,
                    Page::Devices,
                    Page::Services,
                    Page::Startup,
                ]
                .into_iter()
                .map(|page| self.nav_item(page, cx)),
            )
            .child(div().flex_1())
            .child(self.nav_item(Page::Settings, cx))
            .child(
                v_flex()
                    .p_5()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Icon::new(UiIcon::Shield).size(px(14.)))
                            .child(if self.elevated {
                                "管理员权限"
                            } else {
                                "标准用户权限"
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Rust · GPUI Kit 0.7.1"),
                    ),
            )
            .into_any_element()
    }
    fn nav_item(&self, page: Page, cx: &mut Context<Self>) -> AnyElement {
        let active = self.page == page;
        div()
            .id(format!("nav-{}", page.title()))
            .mx_2()
            .my_1()
            .px_4()
            .h(px(42.))
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .cursor_pointer()
            .text_color(if active {
                rgb(0x80b9fa).into()
            } else {
                cx.theme().foreground
            })
            .bg(if active {
                rgb(0x24374d).into()
            } else {
                cx.theme().transparent
            })
            .hover(|s| s.bg(cx.theme().secondary))
            .on_click(cx.listener(move |this, _, w, cx| this.set_page(page, w, cx)))
            .child(Icon::new(page.icon()).size(px(18.)))
            .child(page.title())
            .test_support()
            .into_any_element()
    }
    fn metric_strip(&self, cx: &Context<Self>) -> AnyElement {
        let s = self.snapshot.as_deref();
        let metrics = [
            (
                "CPU",
                percent(s.filter(|s| s.cpu_ready).map(|s| s.cpu_usage)),
                UiIcon::Cpu,
            ),
            (
                "内存",
                percent(s.map(SystemSnapshot::memory_percent)),
                UiIcon::MemoryStick,
            ),
            (
                "GPU",
                percent(s.and_then(SystemSnapshot::gpu_percent)),
                UiIcon::Monitor,
            ),
            (
                "磁盘",
                percent(s.and_then(|s| s.disk_usage)),
                UiIcon::HardDrive,
            ),
            (
                "网络接收",
                rate(s.and_then(|s| s.network_receive_bytes_per_sec)),
                UiIcon::Network,
            ),
        ];
        h_flex()
            .h(px(92.))
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(
                metrics
                    .into_iter()
                    .enumerate()
                    .map(|(i, (name, value, icon))| {
                        v_flex()
                            .flex_1()
                            .px_6()
                            .gap_2()
                            .when(i > 0, |e| e.border_l_1().border_color(cx.theme().border))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(Icon::new(icon).size(px(14.)))
                                    .child(name),
                            )
                            .child(div().font_family("Consolas").text_2xl().child(value))
                    }),
            )
            .into_any_element()
    }
    fn process_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let table = self.processes.read(cx).delegate();
        let p = table.selection().cloned();
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .h(px(50.))
                    .px_6()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} 个进程 · 点击列标题排序", table.rows.len())),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("export")
                                    .ghost()
                                    .small()
                                    .icon(UiIcon::Download)
                                    .label("导出 CSV")
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.export_csv(&ExportCsv, w, cx)
                                    })),
                            )
                            .child(
                                Button::new("end")
                                    .small()
                                    .danger()
                                    .icon(UiIcon::Square)
                                    .label("结束任务")
                                    .disabled(p.is_none())
                                    .on_click(cx.listener(|this, _, w, cx| this.end_task(w, cx))),
                            ),
                    ),
            )
            .when_some(
                self.snapshot.as_ref().and_then(|s| s.gpu_note.clone()),
                |e, note| {
                    e.child(
                        div()
                            .px_6()
                            .py_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(note),
                    )
                },
            )
            .child(
                div().flex_1().min_h_0().child(
                    DataTable::new(&self.processes)
                        .bordered(false)
                        .stripe(false)
                        .small(),
                ),
            )
            .when_some(p, |e, p| e.child(self.process_inspector(p, cx)))
            .into_any_element()
    }
    fn process_inspector(
        &self,
        p: system_monitor_x::monitor::ProcessSample,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = p.exe.as_ref().map(|p| p.to_string_lossy().to_string());
        v_flex()
            .flex_shrink_0()
            .p_4()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary.opacity(0.35))
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(p.name.clone()),
                            )
                            .child(div().text_color(cx.theme().muted_foreground).child(format!(
                                "PID {} · 已运行 {}",
                                p.pid,
                                if p.start_time_unix_secs > 0 {
                                    duration(p.run_time_secs)
                                } else {
                                    "不可用".into()
                                }
                            ))),
                    )
                    .child(
                        Button::new("location")
                            .small()
                            .ghost()
                            .icon(UiIcon::FolderOpen)
                            .label("打开文件位置")
                            .disabled(path.is_none())
                            .on_click(cx.listener({
                                let path = path.clone();
                                move |this, _, _, cx| {
                                    if let Some(path) = &path
                                        && let Err(e) = platform::open_file_location(path)
                                    {
                                        this.message = Some(format!("无法打开位置：{e:#}"));
                                        cx.notify();
                                    }
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(path.unwrap_or_else(|| "系统或受保护进程：路径不可访问".into())),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("进程优先级"),
                    )
                    .children(
                        [
                            ("低", Priority::Idle),
                            ("低于正常", Priority::BelowNormal),
                            ("正常", Priority::Normal),
                            ("高于正常", Priority::AboveNormal),
                            ("高", Priority::High),
                        ]
                        .into_iter()
                        .map(|(name, priority)| {
                            Button::new(format!("priority-{name}"))
                                .small()
                                .ghost()
                                .label(name)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.message = Some(
                                        match platform::set_priority(
                                            p.pid,
                                            p.start_time_unix_secs,
                                            priority,
                                        ) {
                                            Ok(()) => format!("优先级已设为{name}"),
                                            Err(e) => format!("优先级设置失败：{e:#}"),
                                        },
                                    );
                                    cx.notify();
                                }))
                        }),
                    ),
            )
            .into_any_element()
    }
    fn chart(
        &self,
        title: impl Into<SharedString>,
        value: String,
        metric: fn(&HistoryPoint) -> Option<f64>,
        domain: Option<(f64, f64)>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let color: Hsla = rgb(0x69afff).into();
        let data: Vec<(String, f64)> = self
            .history
            .iter()
            .filter_map(|p| metric(p).map(|v| (p.time.clone(), v)))
            .collect();
        let available = !data.is_empty();
        let chart = AreaChart::new(data)
            .x(|d| d.0.clone())
            .y(|d| d.1)
            .stroke(color)
            .fill(color.opacity(0.12))
            .x_axis(false)
            .tick_margin(24)
            .y_tick_count(3)
            .when_some(domain, |c, (lo, hi)| c.y_domain(lo, hi));
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(
                h_flex()
                    .justify_between()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title.into()))
                    .child(
                        div()
                            .font_family("Consolas")
                            .text_lg()
                            .text_color(color)
                            .child(value),
                    ),
            )
            .child(
                div()
                    .h(px(176.))
                    .when(available, |e| e.child(chart))
                    .when(!available, |e| {
                        e.flex()
                            .items_center()
                            .justify_center()
                            .text_color(cx.theme().muted_foreground)
                            .child("等待有效采样")
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("最近 120 次采样"),
            )
            .into_any_element()
    }
    fn performance(&self, cx: &Context<Self>) -> AnyElement {
        let Some(s) = self.snapshot.as_deref() else {
            return div().p_8().child("正在采集系统信息…").into_any_element();
        };
        v_flex()
            .id("performance-scroll")
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .gap_6()
            .child(
                h_flex()
                    .gap_8()
                    .child(self.chart(
                        "CPU",
                        percent(s.cpu_ready.then_some(s.cpu_usage)),
                        |p| Some(p.cpu),
                        Some((0., 100.)),
                        cx,
                    ))
                    .child(self.chart(
                        "内存",
                        format!("{} / {}", bytes(s.memory_used), bytes(s.memory_total)),
                        |p| Some(p.memory),
                        Some((0., 100.)),
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .gap_8()
                    .child(self.chart(
                        "GPU · 最忙引擎",
                        percent(s.gpu_percent()),
                        |p| p.gpu,
                        Some((0., 100.)),
                        cx,
                    ))
                    .child(self.chart(
                        "磁盘 · 活跃时间",
                        percent(s.disk_usage),
                        |p| p.disk,
                        Some((0., 100.)),
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .gap_8()
                    .child(self.chart(
                        "网络接收",
                        rate(s.network_receive_bytes_per_sec),
                        |p| p.net_rx,
                        None,
                        cx,
                    ))
                    .child(self.chart(
                        "网络发送",
                        rate(s.network_transmit_bytes_per_sec),
                        |p| p.net_tx,
                        None,
                        cx,
                    )),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_5()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("逻辑处理器"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} · {} 逻辑处理器 · {} · {}",
                        s.cpu_name,
                        s.logical_cpus,
                        s.physical_cores
                            .map(|n| format!("{n} 物理核心"))
                            .unwrap_or_else(|| "物理核心数不可用".into()),
                        s.cpu_frequency_mhz
                            .map(|n| format!("{:.2} GHz", n as f64 / 1000.))
                            .unwrap_or_else(|| "频率不可用".into())
                    )),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_3()
                    .children(s.cpus.iter().map(|cpu| {
                        v_flex()
                            .w(px(116.))
                            .gap_2()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .text_xs()
                                    .child(format!("CPU {}", cpu.index))
                                    .child(percent(s.cpu_ready.then_some(cpu.usage_percent))),
                            )
                            .child(
                                div().h(px(3.)).w_full().bg(cx.theme().secondary).child(
                                    div()
                                        .h_full()
                                        .w(relative(cpu.usage_percent / 100.))
                                        .bg(rgb(0x69afff)),
                                ),
                            )
                    })),
            )
            .into_any_element()
    }
    fn detail_row(
        label: impl Into<SharedString>,
        value: impl Into<SharedString>,
        cx: &Context<Self>,
    ) -> AnyElement {
        h_flex()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .gap_5()
            .child(
                div()
                    .w(px(174.))
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(label.into()),
            )
            .child(div().flex_1().child(value.into()))
            .into_any_element()
    }
    fn devices(&self, cx: &Context<Self>) -> AnyElement {
        let Some(s) = self.snapshot.as_deref() else {
            return div().p_8().child("正在读取设备…").into_any_element();
        };
        v_flex()
            .id("devices-scroll")
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .gap_6()
            .child(
                v_flex()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("系统信息"),
                    )
                    .child(Self::detail_row("计算机", s.host_name.clone(), cx))
                    .child(Self::detail_row("操作系统", s.os_version.clone(), cx))
                    .child(Self::detail_row("处理器", s.cpu_name.clone(), cx))
                    .child(Self::detail_row(
                        "物理内存",
                        format!(
                            "已用 {} / 总计 {} · 可用 {}",
                            bytes(s.memory_used),
                            bytes(s.memory_total),
                            bytes(s.memory_available)
                        ),
                        cx,
                    ))
                    .child(Self::detail_row(
                        "交换空间",
                        format!("{} / {}", bytes(s.swap_used), bytes(s.swap_total)),
                        cx,
                    ))
                    .child(Self::detail_row("运行时间", duration(s.uptime_secs), cx)),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("图形处理器"),
                    )
                    .children(s.gpus.iter().map(|g| {
                        v_flex()
                            .child(Self::detail_row(
                                g.name.clone(),
                                format!(
                                    "GPU {} · {} · 专用显存 {} / {} · 共享显存 {}",
                                    g.index,
                                    percent(g.usage_percent),
                                    g.dedicated_memory_used
                                        .map(bytes)
                                        .unwrap_or_else(|| "不可用".into()),
                                    bytes(g.dedicated_memory_total),
                                    g.shared_memory_used
                                        .map(bytes)
                                        .unwrap_or_else(|| "不可用".into())
                                ),
                                cx,
                            ))
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .gap_4()
                                    .py_2()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .children(g.engines.iter().map(|engine| {
                                        div().child(format!(
                                            "{} {:.1}%",
                                            engine.name, engine.usage_percent
                                        ))
                                    })),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(g.status.clone()),
                            )
                    }))
                    .when_some(s.gpu_note.clone(), |e, n| {
                        e.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(n),
                        )
                    }),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("存储设备"),
                    )
                    .children(s.disks.iter().map(|d| {
                        Self::detail_row(
                            d.mount_point.clone(),
                            format!(
                                "{} · {} · 可用 {} / {} · 读 {} · 写 {}",
                                d.name,
                                d.kind,
                                bytes(d.available_bytes),
                                bytes(d.total_bytes),
                                rate(d.read_bytes_per_sec),
                                rate(d.write_bytes_per_sec)
                            ),
                            cx,
                        )
                    })),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("网络接口"),
                    )
                    .children(s.networks.iter().map(|n| {
                        Self::detail_row(
                            n.name.clone(),
                            format!(
                                "{} · {} · 接收 {} · 发送 {} · 累计接收 {}",
                                n.state,
                                n.mac_address,
                                rate(n.received_bytes_per_sec),
                                rate(n.transmitted_bytes_per_sec),
                                bytes(n.total_received_bytes)
                            ),
                            cx,
                        )
                    })),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("温度传感器"),
                    )
                    .children(s.temperatures.iter().map(|t| {
                        Self::detail_row(
                            t.label.clone(),
                            t.celsius
                                .map(|v| format!("{v:.1} °C"))
                                .unwrap_or_else(|| "不可用".into()),
                            cx,
                        )
                    }))
                    .when_some(s.temperature_note.clone(), |e, n| {
                        e.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(n),
                        )
                    }),
            )
            .into_any_element()
    }
    fn settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex().id("settings-scroll").size_full().overflow_y_scroll().p_8().gap_6().child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("采样与外观"))
            .child(h_flex().gap_6().child(div().w(px(150.)).child("刷新间隔")).children([(500,"0.5 秒"),(1000,"1 秒"),(2000,"2 秒")].into_iter().map(|(ms,label)|Button::new(format!("interval-{ms}")).label(label).when(self.settings.interval_ms==ms,|b|b.primary()).on_click(cx.listener(move|this,_,_,cx|{this.settings.interval_ms=ms;this.interval.store(ms,Ordering::Relaxed);if let Err(e)=this.settings.save(){this.message=Some(format!("设置保存失败：{e}"));}cx.notify();})))))
            .child(h_flex().gap_6().child(div().w(px(150.)).child("主题")).child(Button::new("theme").label(if self.settings.dark {"切换为浅色"}else{"切换为深色"}).on_click(cx.listener(|this,_,w,cx|{this.settings.dark = !this.settings.dark;Theme::change(if this.settings.dark{ThemeMode::Dark}else{ThemeMode::Light},Some(w),cx);if let Err(e)=this.settings.save(){this.message=Some(format!("设置保存失败：{e}"));}cx.notify();}))))
            .child(div().pt_5().border_t_1().border_color(cx.theme().border).text_lg().font_weight(FontWeight::SEMIBOLD).child("快捷键")).child(Self::detail_row("Ctrl + F","搜索",cx)).child(Self::detail_row("Ctrl + N","运行新任务",cx)).child(Self::detail_row("Ctrl + E","导出进程 CSV",cx)).child(Self::detail_row("Ctrl + P","暂停 / 恢复显示",cx))
            .child(div().pt_5().text_lg().font_weight(FontWeight::SEMIBOLD).child("关于 SystemMonitorX")).child(div().text_color(cx.theme().muted_foreground).child("v0.1.0 · 原生 Rust · Longbridge GPUI Kit 0.7.1 · Windows 10 / 11 x64"))
            .child(div().text_color(cx.theme().muted_foreground).child("数据来自 sysinfo、DXGI 和 Windows PDH。GPU 使用率显示最忙引擎；显存来自系统整卡计数器。温度与受保护进程的数据由驱动及访问权限决定，不可用的数据以 — 标记。"))
            .child(Button::new("github").ghost().label("在 GitHub 查看源码").on_click(|_,_,cx|cx.open_url("https://github.com/neko233-com/SystemMonitorX"))).into_any_element()
    }
}
impl Render for SystemMonitorApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .font_family("Microsoft YaHei UI")
            .text_sm()
            .text_color(cx.theme().foreground)
            .bg(cx.theme().background)
            .key_context("SystemMonitorX")
            .id("app-root")
            .test_support()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::toggle_pause))
            .on_action(cx.listener(Self::new_task))
            .on_action(cx.listener(Self::export_csv))
            .child(self.sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        h_flex()
                            .h(px(76.))
                            .flex_shrink_0()
                            .px_6()
                            .gap_5()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(self.page.title()),
                            )
                            .child(div().flex_1())
                            .when(
                                matches!(
                                    self.page,
                                    Page::Processes | Page::Services | Page::Startup
                                ),
                                |e| {
                                    e.child(
                                        div().w(px(310.)).child(
                                            Input::new(&self.search)
                                                .prefix(Icon::new(UiIcon::Search).size(px(16.)))
                                                .small(),
                                        ),
                                    )
                                },
                            )
                            .child(
                                Button::new("pause")
                                    .ghost()
                                    .small()
                                    .icon(if self.paused {
                                        UiIcon::Play
                                    } else {
                                        UiIcon::Pause
                                    })
                                    .tooltip("Ctrl + P")
                                    .label(if self.paused { "恢复" } else { "暂停" })
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.toggle_pause(&TogglePause, w, cx)
                                    })),
                            )
                            .child(
                                Button::new("new-task")
                                    .small()
                                    .icon(UiIcon::Plus)
                                    .label("运行新任务")
                                    .on_click(
                                        cx.listener(|this, _, w, cx| {
                                            this.new_task(&NewTask, w, cx)
                                        }),
                                    ),
                            ),
                    )
                    .when(
                        matches!(self.page, Page::Processes | Page::Performance),
                        |e| e.child(self.metric_strip(cx)),
                    )
                    .when_some(self.message.clone(), |e, message| {
                        e.child(
                            h_flex()
                                .px_6()
                                .py_2()
                                .bg(cx.theme().secondary)
                                .justify_between()
                                .child(div().flex_1().child(message))
                                .child(
                                    Button::new("dismiss")
                                        .ghost()
                                        .small()
                                        .icon(UiIcon::X)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.message = None;
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
                    .child(
                        div().flex_1().min_h_0().child(match self.page {
                            Page::Processes => self.process_view(cx),
                            Page::Performance => self.performance(cx),
                            Page::Devices => self.devices(cx),
                            Page::Settings => self.settings_view(cx),
                            Page::Services | Page::Startup => v_flex()
                                .size_full()
                                .min_h_0()
                                .child(
                                    h_flex()
                                        .h(px(52.))
                                        .px_6()
                                        .justify_between()
                                        .child(div().text_color(cx.theme().muted_foreground).child(
                                            if self.page == Page::Services {
                                                "Windows 服务 · 只读列表"
                                            } else {
                                                "登录时启动的应用 · 注册表和启动文件夹"
                                            },
                                        ))
                                        .child(
                                            Button::new("refresh-metadata")
                                                .small()
                                                .ghost()
                                                .label(if self.metadata_loading {
                                                    "读取中…"
                                                } else {
                                                    "刷新列表"
                                                })
                                                .disabled(self.metadata_loading)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.refresh_metadata(cx)
                                                })),
                                        ),
                                )
                                .child(
                                    div().flex_1().min_h_0().child(
                                        DataTable::new(if self.page == Page::Services {
                                            &self.services
                                        } else {
                                            &self.startup
                                        })
                                        .bordered(false)
                                        .small(),
                                    ),
                                )
                                .into_any_element(),
                        }),
                    )
                    .child(
                        h_flex()
                            .h(px(32.))
                            .px_6()
                            .flex_shrink_0()
                            .gap_2()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(div().size(px(6.)).rounded_full().bg(if self.paused {
                                rgb(0xf3b95f)
                            } else {
                                rgb(0x6ac794)
                            }))
                            .child(if self.paused {
                                "显示已暂停"
                            } else if self.snapshot.is_none() {
                                "正在连接系统…"
                            } else {
                                "实时采集"
                            })
                            .child(format!(
                                "· {:.1} 秒",
                                self.settings.interval_ms as f64 / 1000.
                            ))
                            .child(div().flex_1())
                            .child(
                                self.snapshot
                                    .as_ref()
                                    .map(|s| {
                                        format!(
                                            "{} · 运行时间 {}",
                                            s.host_name,
                                            duration(s.uptime_secs)
                                        )
                                    })
                                    .unwrap_or_else(|| "Windows x64".into()),
                            ),
                    ),
            )
    }
}
pub fn run() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::component::set_locale("zh-CN");
            cx.bind_keys([
                KeyBinding::new("ctrl-f", FocusSearch, Some("SystemMonitorX")),
                KeyBinding::new("ctrl-p", TogglePause, Some("SystemMonitorX")),
                KeyBinding::new("ctrl-n", NewTask, Some("SystemMonitorX")),
                KeyBinding::new("ctrl-e", ExportCsv, Some("SystemMonitorX")),
            ]);
            let testing =
                cfg!(feature = "ui-test") && std::env::args().any(|arg| arg == "--ui-smoke-dir");
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1380.), px(860.)), cx)),
                window_min_size: Some(size(px(1040.), px(640.))),
                ..Default::default()
            };
            let (handle, view) = gpui_kit::open_window(options, cx, |w, cx| {
                w.set_window_title("SystemMonitorX");
                if !testing {
                    w.activate_window();
                }
                cx.new(|cx| SystemMonitorApp::new(w, cx))
            })
            .expect("open SystemMonitorX window");
            #[cfg(feature = "ui-test")]
            if let Some(directory) = std::env::args()
                .skip_while(|arg| arg != "--ui-smoke-dir")
                .nth(1)
            {
                qa::start(handle, view, PathBuf::from(directory), cx);
            }
            #[cfg(not(feature = "ui-test"))]
            let _ = (handle, view);
            if !testing {
                cx.activate(true);
            }
        });
}
