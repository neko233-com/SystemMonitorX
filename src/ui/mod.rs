mod columns;
mod directx;
mod hardware;
mod management;
mod process_details;
#[cfg(feature = "ui-test")]
mod qa;
#[cfg(feature = "ui-test")]
mod qa_performance;
#[cfg(feature = "ui-test")]
mod qa_update;
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
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
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
    process_details::ProcessDetails,
    updater::{UpdatePhase, UpdateService, UpdateSnapshot},
};
use tables::{ProcessTable, TextTable, bytes, duration, percent, rate};

gpui_kit::actions!(
    system_monitor_x,
    [FocusSearch, TogglePause, NewTask, ExportCsv]
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Processes,
    Details,
    Performance,
    History,
    Users,
    Devices,
    Services,
    Startup,
    Settings,
}
impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Processes => "进程",
            Self::Details => "详细信息",
            Self::Performance => "性能",
            Self::History => "应用历史",
            Self::Users => "用户",
            Self::Devices => "设备与传感器",
            Self::Services => "服务",
            Self::Startup => "启动应用",
            Self::Settings => "设置",
        }
    }
    fn icon(self) -> UiIcon {
        match self {
            Self::Processes => UiIcon::Layers,
            Self::Details => UiIcon::Layers,
            Self::Performance => UiIcon::Activity,
            Self::History => UiIcon::Activity,
            Self::Users => UiIcon::Shield,
            Self::Devices => UiIcon::Cpu,
            Self::Services => UiIcon::Settings2,
            Self::Startup => UiIcon::Rocket,
            Self::Settings => UiIcon::Settings,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    interval_ms: u64,
    dark: bool,
    auto_updates: bool,
    always_on_top: bool,
    hidden_process_columns: Vec<usize>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            dark: true,
            auto_updates: true,
            always_on_top: false,
            hidden_process_columns: Vec::new(),
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
        s.hidden_process_columns
            .retain(|column| *column > 0 && *column < 18);
        s.hidden_process_columns.sort_unstable();
        s.hidden_process_columns.dedup();
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
enum CollectorCommand {
    ResetHistory(std::sync::mpsc::Sender<std::result::Result<(), String>>),
}
type SnapshotSlot = Arc<Mutex<Option<(u64, Arc<SystemSnapshot>)>>>;

pub struct SystemMonitorApp {
    page: Page,
    snapshot: Option<Arc<SystemSnapshot>>,
    slot: SnapshotSlot,
    collector_commands: std::sync::mpsc::SyncSender<CollectorCommand>,
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
    users: Entity<TableState<TextTable>>,
    app_history: Entity<TableState<TextTable>>,
    service_entries: Vec<platform::ServiceInfo>,
    startup_entries: Vec<platform::StartupEntry>,
    history: VecDeque<HistoryPoint>,
    sample_index: u64,
    message: Option<String>,
    elevated: bool,
    metadata_loading: bool,
    updater: UpdateService,
    update_state: UpdateSnapshot,
    last_install_result: Option<system_monitor_x::update_install::InstallResult>,
    operation_busy: bool,
    install_after_download: bool,
    hardware: Option<Arc<system_monitor_x::hardware::HardwareInventory>>,
    hardware_sections: Vec<hardware::HardwareSection>,
    hardware_loading: bool,
    hardware_error: Option<String>,
    directx_inventory: Option<Arc<system_monitor_x::directx::DirectXInventory>>,
    directx_loading: bool,
    directx_error: Option<String>,
    process_details: Option<Arc<ProcessDetails>>,
    process_details_loading: bool,
    process_details_error: Option<String>,
    process_details_identity: Option<(u32, u64)>,
    process_details_expanded: bool,
    process_details_generation: u64,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
    settings_writer: std::sync::mpsc::Sender<Settings>,
    settings_error: Arc<Mutex<Option<String>>>,
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
        let (collector_commands, worker_commands) =
            std::sync::mpsc::sync_channel::<CollectorCommand>(4);
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
                    while let Ok(command) = worker_commands.try_recv() {
                        match command {
                            CollectorCommand::ResetHistory(reply) => {
                                let _ = reply
                                    .send(monitor.reset_history().map_err(|e| format!("{e:#}")));
                            }
                        }
                    }
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
            let mut delegate = ProcessTable::new();
            delegate.set_hidden_columns(&settings.hidden_process_columns);
            TableState::new(delegate, window, cx)
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
                ])
                .numeric_columns(&[3]),
                window,
                cx,
            )
            .col_selectable(false)
            .col_movable(false)
        });
        let startup = cx.new(|cx| {
            TableState::new(
                TextTable::new(&[
                    ("名称", 240.),
                    ("状态", 100.),
                    ("命令 / 路径", 610.),
                    ("来源", 260.),
                ]),
                window,
                cx,
            )
            .col_selectable(false)
            .col_movable(false)
        });
        let users = cx.new(|cx| {
            TableState::new(
                TextTable::new(&[
                    ("用户", 240.),
                    ("会话 ID", 95.),
                    ("状态", 120.),
                    ("进程", 80.),
                    ("CPU", 95.),
                    ("工作集合计", 125.),
                    ("GPU", 95.),
                    ("客户端 / 协议", 230.),
                ])
                .numeric_columns(&[1, 3, 4, 5, 6]),
                window,
                cx,
            )
            .col_selectable(false)
        });
        let app_history = cx.new(|cx| {
            TableState::new(
                TextTable::new(&[
                    ("应用", 240.),
                    ("CPU 累计", 130.),
                    ("I/O 读取", 125.),
                    ("I/O 写入", 125.),
                    ("网络接收", 125.),
                    ("网络发送", 125.),
                    ("网络有效进程秒", 150.),
                    ("实例 / 活跃", 125.),
                    ("路径", 650.),
                ])
                .numeric_columns(&[1, 2, 3, 4, 5, 6]),
                window,
                cx,
            )
            .col_selectable(false)
        });
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filter(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&processes, |this, table, event: &TableEvent, cx| {
                match event {
                    TableEvent::SelectRow(i) | TableEvent::DoubleClickedRow(i) => {
                        let id = table
                            .read(cx)
                            .delegate()
                            .row(*i)
                            .map(tables::process_identity);
                        table.update(cx, |t, _| t.delegate_mut().selected = id);
                        if let Some((pid, ticks)) = id
                            && this.process_details_identity != id
                        {
                            this.refresh_process_details(pid, ticks, cx);
                        }
                    }
                    TableEvent::ColumnWidthsChanged(widths) => {
                        table.update(cx, |t, _| t.delegate_mut().set_column_widths(widths));
                    }
                    TableEvent::ClearSelection => {
                        table.update(cx, |t, _| t.delegate_mut().selected = None);
                        this.process_details_identity = None;
                        this.process_details = None;
                        this.process_details_error = None;
                        this.process_details_loading = false;
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
        let updater = UpdateService::new(settings.auto_updates);
        let update_state = updater.snapshot();
        let (settings_writer, settings_receiver) = std::sync::mpsc::channel::<Settings>();
        let settings_error = Arc::new(Mutex::new(None));
        let worker_settings_error = settings_error.clone();
        std::thread::Builder::new()
            .name("settings-writer".into())
            .spawn(move || {
                while let Ok(mut settings) = settings_receiver.recv() {
                    while let Ok(latest) = settings_receiver.try_recv() {
                        settings = latest;
                    }
                    if let Err(error) = settings.save()
                        && let Ok(mut slot) = worker_settings_error.lock()
                    {
                        *slot = Some(format!("设置保存失败：{error}"));
                    }
                }
            })
            .expect("start settings writer");
        let mut app = Self {
            page: Page::Processes,
            snapshot: None,
            slot,
            collector_commands,
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
            users,
            app_history,
            service_entries: vec![],
            startup_entries: vec![],
            history: VecDeque::new(),
            sample_index: 0,
            message: None,
            elevated: platform::is_elevated().unwrap_or(false),
            metadata_loading: false,
            updater,
            update_state,
            last_install_result: None,
            operation_busy: false,
            install_after_download: false,
            hardware: None,
            hardware_sections: Vec::new(),
            hardware_loading: false,
            hardware_error: None,
            directx_inventory: None,
            directx_loading: false,
            directx_error: None,
            process_details: None,
            process_details_loading: false,
            process_details_error: None,
            process_details_identity: None,
            process_details_expanded: false,
            process_details_generation: 0,
            focus_handle,
            _subscriptions: subscriptions,
            _poll: poll,
            settings_writer,
            settings_error,
        };
        app.refresh_metadata(cx);
        app.refresh_hardware(cx);
        app.refresh_directx(cx);
        cx.spawn(async move |view, cx| {
            let result = smol::unblock(system_monitor_x::update_install::read_last_result).await;
            let _ = view.update(cx, |this, cx| {
                match result {
                    Ok(result) => this.last_install_result = result,
                    Err(error) => this.message = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        if app.settings.always_on_top
            && let Err(error) = Self::apply_topmost(window, true)
        {
            app.message = Some(error.to_string());
        }
        app
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        if let Some(error) = self
            .settings_error
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
        {
            self.message = Some(error);
            cx.notify();
        }
        let update = self.updater.snapshot();
        if update.sequence != self.update_state.sequence {
            self.update_state = update;
            cx.notify();
        }
        if self.install_after_download {
            match self.update_state.phase {
                UpdatePhase::Ready(_) if !self.operation_busy => {
                    self.install_after_download = false;
                    self.install_update(cx);
                }
                UpdatePhase::Failed(_) => self.install_after_download = false,
                _ => {}
            }
        }
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
            d.set_snapshot(snapshot.clone());
            tables::sync_selection(table, cx);
            cx.notify();
        });
        self.users.update(cx, |table, cx| {
            let selected = table
                .selected_row()
                .and_then(|i| table.delegate().rows.get(i))
                .and_then(|r| r.get(1))
                .cloned();
            let d = table.delegate_mut();
            d.all = snapshot
                .sessions
                .iter()
                .filter(|s| s.user_name.as_ref().is_some_and(|u| !u.is_empty()))
                .map(|s| {
                    vec![
                        format!(
                            "{}\\{}",
                            s.domain.as_deref().unwrap_or(""),
                            s.user_name.as_deref().unwrap_or("")
                        ),
                        s.id.to_string(),
                        s.state.clone(),
                        s.process_count.to_string(),
                        format!("{:.1}%", s.cpu_usage),
                        bytes(s.memory_bytes),
                        percent(s.gpu_usage),
                        format!(
                            "{} · {}",
                            s.client_name.as_deref().unwrap_or("本机"),
                            s.protocol.as_deref().unwrap_or("—")
                        ),
                    ]
                })
                .collect();
            d.rebuild();
            if let Some(id) = selected {
                let next = d.rows.iter().position(|r| r[1] == id);
                if let Some(next) = next {
                    table.set_selected_row(next, cx)
                } else {
                    table.clear_selection(cx)
                }
            }
            cx.notify();
        });
        self.app_history.update(cx, |table, cx| {
            let d = table.delegate_mut();
            d.all = snapshot
                .app_history
                .iter()
                .map(|h| {
                    vec![
                        h.name.clone(),
                        h.cpu_seconds
                            .map(|n| format!("{n:.2} s"))
                            .unwrap_or_else(|| "—".into()),
                        h.read_bytes.map(bytes).unwrap_or_else(|| "—".into()),
                        h.write_bytes.map(bytes).unwrap_or_else(|| "—".into()),
                        h.network_received_bytes
                            .map(bytes)
                            .unwrap_or_else(|| "—".into()),
                        h.network_transmitted_bytes
                            .map(bytes)
                            .unwrap_or_else(|| "—".into()),
                        h.network_covered_seconds
                            .map(|n| format!("{n:.1} s"))
                            .unwrap_or_else(|| "—".into()),
                        format!("{} / {}", h.process_instances, h.running_instances),
                        h.exe
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|| "—".into()),
                    ]
                })
                .collect();
            d.rebuild();
            cx.notify();
        });
        self.snapshot = Some(snapshot);
        cx.notify();
    }
    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_string();
        if matches!(self.page, Page::Processes | Page::Details) {
            self.processes.update(cx, |t, cx| {
                let changed = t.delegate().query != query;
                if !changed {
                    return;
                }
                t.delegate_mut().query = query;
                t.delegate_mut().rebuild();
                tables::sync_selection(t, cx);
                if !t.delegate().rows.is_empty() {
                    t.scroll_to_row(0, cx);
                }
                cx.notify();
            });
            return;
        }
        let table = match self.page {
            Page::Services => Some(&self.services),
            Page::Startup => Some(&self.startup),
            Page::Users => Some(&self.users),
            Page::History => Some(&self.app_history),
            _ => None,
        };
        if let Some(table) = table {
            table.update(cx, |t, cx| {
                if t.delegate().query == query {
                    return;
                }
                t.delegate_mut().query = query;
                t.delegate_mut().rebuild();
                t.scroll_to_row(0, cx);
                cx.notify();
            });
        }
    }
    fn set_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        self.processes.update(cx, |table, cx| {
            table.delegate_mut().detailed = page == Page::Details;
            table.refresh(cx);
        });
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        // Programmatic InputState::set_value does not emit InputEvent::Change.
        // Reset the newly visible model; hidden pages are not searched on each key.
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
                    Ok(rows) => {
                        this.service_entries = rows.clone();
                        this.services.update(cx, |t, cx| {
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
                            t.clear_selection(cx);
                            cx.notify();
                        });
                    }
                    Err(e) => this.message = Some(format!("服务读取失败：{e:#}")),
                }
                match results.1 {
                    Ok(rows) => {
                        this.startup_entries = rows.clone();
                        this.startup.update(cx, |t, cx| {
                            t.delegate_mut().all = rows
                                .into_iter()
                                .map(|s| {
                                    vec![
                                        s.name,
                                        s.enabled
                                            .map(|v| if v { "已启用" } else { "已禁用" }.to_owned())
                                            .unwrap_or_else(|| "不可用".into()),
                                        s.command,
                                        s.source,
                                    ]
                                })
                                .collect();
                            t.delegate_mut().rebuild();
                            t.clear_selection(cx);
                            cx.notify();
                        });
                    }
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
    fn run_operation(
        &mut self,
        label: &str,
        operation: impl FnOnce() -> anyhow::Result<String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.operation_busy {
            self.message = Some("有一项操作正在执行，请稍候".into());
            cx.notify();
            return;
        }
        self.operation_busy = true;
        self.message = Some(format!("{label}…"));
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = smol::unblock(operation).await;
            let _ = view.update(cx, |this, cx| {
                this.operation_busy = false;
                this.message = Some(match result {
                    Ok(message) => message,
                    Err(error) => format!("操作失败：{error:#}"),
                });
                if matches!(this.page, Page::Services | Page::Startup) {
                    this.refresh_metadata(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn install_update(&mut self, cx: &mut Context<Self>) {
        if self.operation_busy {
            return;
        }
        let (UpdatePhase::Ready(path), Some(release)) =
            (&self.update_state.phase, &self.update_state.release)
        else {
            return;
        };
        let (path, release) = (path.clone(), release.clone());
        self.operation_busy = true;
        self.message = Some("正在重新校验安装包…".into());
        cx.spawn(async move |view, cx| {
            let result =
                smol::unblock(move || system_monitor_x::updater::launch_installer(&path, &release))
                    .await;
            let success = result.is_ok();
            let _ = view.update(cx, |this, cx| {
                this.operation_busy = false;
                if let Err(error) = result {
                    this.message = Some(format!("启动安装器失败：{error:#}"));
                }
                cx.notify();
            });
            if success {
                cx.update(|cx| cx.quit());
            }
        })
        .detach();
        cx.notify();
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
        if command.trim().is_empty() {
            let _ = view.update(cx, |this, cx| {
                this.message = Some("启动失败：请输入任务命令".into());
                cx.notify();
            });
            return false;
        }
        let _ = view.update(cx, |this, cx| {
            this.run_operation(
                "正在启动任务",
                move || {
                    platform::launch_task(&command)?;
                    Ok("任务已启动".into())
                },
                cx,
            );
        });
        true
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
                        let _ = view.update(cx, |this, cx| {
                            this.run_operation(
                                "正在结束进程",
                                move || {
                                    platform::terminate_process(
                                        p.pid,
                                        p.start_time_windows_ticks.unwrap_or(0),
                                    )?;
                                    Ok("进程已结束".into())
                                },
                                cx,
                            );
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
            .child(
                v_flex()
                    .id("sidebar-nav")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(
                        [
                            Page::Processes,
                            Page::Performance,
                            Page::History,
                            Page::Devices,
                            Page::Users,
                            Page::Details,
                            Page::Services,
                            Page::Startup,
                        ]
                        .into_iter()
                        .map(|page| self.nav_item(page, cx)),
                    ),
            )
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
                                Button::new("process-columns")
                                    .ghost()
                                    .small()
                                    .label("选择列")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.columns_dialog(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("tree-mode")
                                    .ghost()
                                    .small()
                                    .label(if table.tree_mode {
                                        "列表视图"
                                    } else {
                                        "进程树"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.processes.update(cx, |table, cx| {
                                            let d = table.delegate_mut();
                                            d.tree_mode = !d.tree_mode;
                                            d.rebuild();
                                            tables::sync_selection(table, cx);
                                            cx.notify();
                                        });
                                        cx.notify();
                                    })),
                            )
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
            .when_some(
                self.snapshot
                    .as_ref()
                    .filter(|s| !s.process_network_available)
                    .map(|s| s.process_network_status.clone()),
                |e, note| {
                    e.child(
                        div()
                            .px_6()
                            .py_1()
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
            .id("process-inspector-scroll")
            .max_h(px(300.))
            .overflow_y_scroll()
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
                                    if let Some(path) = path.clone() {
                                        this.run_operation(
                                            "正在打开文件位置",
                                            move || {
                                                platform::open_file_location(&path)?;
                                                Ok("文件位置已打开".into())
                                            },
                                            cx,
                                        );
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
                                    this.run_operation(
                                        "正在设置优先级",
                                        move || {
                                            platform::set_priority(
                                                p.pid,
                                                p.start_time_windows_ticks.unwrap_or(0),
                                                priority,
                                            )?;
                                            Ok(format!("优先级已设为{name}"))
                                        },
                                        cx,
                                    );
                                }))
                        }),
                    ),
            )
            .child(self.extra_process_controls(p, cx))
            .child(self.selected_process_details(cx))
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
                    .pt_5()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("系统内存与对象"),
            )
            .children(
                [
                    (
                        "已提交 / 提交上限",
                        format!(
                            "{} / {}",
                            s.committed_bytes.map(bytes).unwrap_or_else(|| "—".into()),
                            s.commit_limit_bytes
                                .map(bytes)
                                .unwrap_or_else(|| "—".into())
                        ),
                    ),
                    (
                        "提交峰值",
                        s.commit_peak_bytes.map(bytes).unwrap_or_else(|| "—".into()),
                    ),
                    (
                        "系统缓存",
                        s.cached_memory_bytes
                            .map(bytes)
                            .unwrap_or_else(|| "—".into()),
                    ),
                    (
                        "分页池 / 非分页池",
                        format!(
                            "{} / {}",
                            s.kernel_paged_bytes
                                .map(bytes)
                                .unwrap_or_else(|| "—".into()),
                            s.kernel_nonpaged_bytes
                                .map(bytes)
                                .unwrap_or_else(|| "—".into())
                        ),
                    ),
                    (
                        "进程 / 线程 / 句柄",
                        format!(
                            "{} / {} / {}",
                            s.system_process_count
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "—".into()),
                            s.system_thread_count
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "—".into()),
                            s.system_handle_count
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "—".into())
                        ),
                    ),
                    ("本次采集耗时", format!("{:.2} ms", s.collector_duration_ms)),
                ]
                .into_iter()
                .map(|(label, value)| Self::detail_row(label, value, cx)),
            )
            .when_some(s.process_collection_note.clone(), |e, note| {
                e.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(note),
                )
            })
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
    fn devices(&self, cx: &mut Context<Self>) -> AnyElement {
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
                            .child(Self::detail_row(
                                "温度 / 风扇",
                                format!(
                                    "{} / {}",
                                    g.temperature_celsius
                                        .map(|n| format!("{n:.1} °C"))
                                        .unwrap_or_else(|| "—".into()),
                                    g.fan_percent
                                        .map(|n| format!("{n}%"))
                                        .unwrap_or_else(|| "—".into())
                                ),
                                cx,
                            ))
                            .child(Self::detail_row(
                                "功耗 / 上限",
                                format!(
                                    "{} / {}",
                                    g.power_watts
                                        .map(|n| format!("{n:.1} W"))
                                        .unwrap_or_else(|| "—".into()),
                                    g.power_limit_watts
                                        .map(|n| format!("{n:.1} W"))
                                        .unwrap_or_else(|| "—".into())
                                ),
                                cx,
                            ))
                            .child(Self::detail_row(
                                "核心 / 显存频率",
                                format!(
                                    "{} / {}",
                                    g.core_clock_mhz
                                        .map(|n| format!("{n} MHz"))
                                        .unwrap_or_else(|| "—".into()),
                                    g.memory_clock_mhz
                                        .map(|n| format!("{n} MHz"))
                                        .unwrap_or_else(|| "—".into())
                                ),
                                cx,
                            ))
                            .child(Self::detail_row(
                                "PCI / 驱动",
                                format!(
                                    "{} · {} · PCIe {} × {}",
                                    g.pci_address
                                        .map(|p| p.to_string())
                                        .unwrap_or_else(|| "—".into()),
                                    g.driver_version.as_deref().unwrap_or("—"),
                                    g.pcie_generation
                                        .map(|n| n.to_string())
                                        .unwrap_or_else(|| "—".into()),
                                    g.pcie_width
                                        .map(|n| n.to_string())
                                        .unwrap_or_else(|| "—".into())
                                ),
                                cx,
                            ))
                            .child(Self::detail_row(
                                "VBIOS / 传感器来源",
                                format!(
                                    "{} · {}",
                                    g.vbios_version.as_deref().unwrap_or("—"),
                                    g.sensor_source.as_deref().unwrap_or("—")
                                ),
                                cx,
                            ))
                            .when_some(g.sensor_note.clone(), |e, note| {
                                e.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(note),
                                )
                            })
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
            .child(self.directx_details(cx))
            .child(self.hardware_details(cx))
            .into_any_element()
    }
    fn update_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = matches!(
            self.update_state.phase,
            UpdatePhase::Checking | UpdatePhase::Downloading { .. }
        );
        let status = match &self.update_state.phase {
            UpdatePhase::Idle => "等待后台检查".to_owned(),
            UpdatePhase::Checking => "正在后台检查更新…".into(),
            UpdatePhase::Current => "已是最新稳定版本".into(),
            UpdatePhase::Available => format!(
                "发现新版本 v{}",
                self.update_state
                    .release
                    .as_ref()
                    .map(|r| r.version.as_str())
                    .unwrap_or("—")
            ),
            UpdatePhase::Downloading { received, total } => {
                format!("正在下载：{} / {}", bytes(*received), bytes(*total))
            }
            UpdatePhase::Ready(_) => "安装包已通过 SHA-256 校验，可以安装".into(),
            UpdatePhase::Failed(error) => format!("更新检查/下载暂不可用：{error}"),
        };
        let newer = self.update_state.release.as_ref().is_some_and(|r| {
            system_monitor_x::updater::is_newer(&r.version, env!("CARGO_PKG_VERSION"))
                .unwrap_or(false)
        });
        v_flex().gap_3()
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("应用更新"))
            .child(h_flex().gap_3()
                .child(Button::new("auto-updates").label(if self.settings.auto_updates { "后台自动检查：开启" } else { "后台自动检查：关闭" }).on_click(cx.listener(|this, _, _, cx| {
                    this.settings.auto_updates = !this.settings.auto_updates;
                    this.updater.set_enabled(this.settings.auto_updates);
                    this.update_state.enabled = this.settings.auto_updates;
                    let _ = this.settings_writer.send(this.settings.clone());
                    cx.notify();
                })))
                .child(Button::new("check-update").label("立即检查").disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                    this.updater.check();
                    this.update_state.phase = UpdatePhase::Checking;
                    cx.notify();
                }))))
            .child(div().text_sm().child(status))
            .when_some(self.update_state.checked_at, |e, checked| {
                let ago = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs().saturating_sub(checked);
                e.child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("上次成功检查：{} 秒前", ago)))
            })
            .when(newer && !matches!(self.update_state.phase, UpdatePhase::Ready(_)), |e| {
                e.child(Button::new("download-update").primary().label("一键更新").disabled(busy || self.operation_busy).on_click(cx.listener(|this, _, _, cx| {
                    this.install_after_download = true;
                    this.updater.download();
                    this.update_state.phase = UpdatePhase::Downloading { received: 0, total: this.update_state.release.as_ref().map(|r| r.installer_bytes).unwrap_or(0) };
                    cx.notify();
                })))
            })
            .when(matches!(self.update_state.phase, UpdatePhase::Ready(_)), |e|
                e.child(Button::new("install-update").primary().label("一键更新 · 安装已校验版本").disabled(self.operation_busy).on_click(cx.listener(|this, _, _, cx| this.install_update(cx)))))
            .when_some(self.last_install_result.clone(), |e, result| {
                let log = result.log_path.to_string_lossy().into_owned();
                let mut details = v_flex().gap_1()
                    .child(div().text_sm().child(format!("上次安装 v{}：{}", result.version, result.message)));
                if let Some(error) = result.restart_error {
                    details = details.child(div().text_xs().child(format!("重新启动失败：{error}")));
                }
                e.child(details.child(Button::new("install-log").ghost().small().label("查看安装日志位置")
                    .disabled(self.operation_busy).on_click(cx.listener(move |this, _, _, cx| {
                        let log = log.clone();
                        this.run_operation("正在打开安装日志", move || {
                            platform::open_file_location(&log)?;
                            Ok("安装日志位置已打开".into())
                        }, cx);
                    }))))
            })
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("应用运行时每 6 小时后台检查稳定版；网络失败后自动重试。点击“一键更新”后自动下载、校验，由独立安装助手完成升级并重新启动。便携版也会安装到本用户的应用目录。"))
            .into_any_element()
    }
    fn apply_topmost(window: &Window, enabled: bool) -> anyhow::Result<()> {
        let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)
            .map_err(|e| anyhow::anyhow!("无法读取原生窗口句柄：{e:?}"))?
            .as_raw()
        else {
            anyhow::bail!("当前窗口不是 Windows 原生窗口");
        };
        system_monitor_x::runtime::set_window_topmost(handle.hwnd.get(), enabled)
    }
    fn restart_as_admin(&mut self, cx: &mut Context<Self>) {
        if self.operation_busy || self.elevated {
            return;
        }
        self.operation_busy = true;
        self.message = Some("等待 Windows 管理员权限确认…".into());
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = smol::unblock(system_monitor_x::runtime::relaunch_elevated).await;
            let success = result.is_ok();
            let _ = view.update(cx, |this, cx| {
                this.operation_busy = false;
                if let Err(error) = result {
                    this.message = Some(format!("{error:#}"));
                }
                cx.notify();
            });
            if success {
                cx.update(|cx| cx.quit());
            }
        })
        .detach();
    }
    fn settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex().id("settings-scroll").size_full().overflow_y_scroll().p_8().gap_6().child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("采样与外观"))
            .child(h_flex().gap_6().child(div().w(px(150.)).child("刷新间隔")).children([(500,"0.5 秒"),(1000,"1 秒"),(2000,"2 秒")].into_iter().map(|(ms,label)|Button::new(format!("interval-{ms}")).label(label).when(self.settings.interval_ms==ms,|b|b.primary()).on_click(cx.listener(move|this,_,_,cx|{this.settings.interval_ms=ms;this.interval.store(ms,Ordering::Relaxed);let _=this.settings_writer.send(this.settings.clone());cx.notify();})))))
            .child(h_flex().gap_6().child(div().w(px(150.)).child("主题")).child(Button::new("theme").label(if self.settings.dark {"切换为浅色"}else{"切换为深色"}).on_click(cx.listener(|this,_,w,cx|{this.settings.dark = !this.settings.dark;Theme::change(if this.settings.dark{ThemeMode::Dark}else{ThemeMode::Light},Some(w),cx);let _=this.settings_writer.send(this.settings.clone());cx.notify();}))))
            .child(div().pt_5().border_t_1().border_color(cx.theme().border).text_lg().font_weight(FontWeight::SEMIBOLD).child("快捷键")).child(Self::detail_row("Ctrl + F","搜索",cx)).child(Self::detail_row("Ctrl + N","运行新任务",cx)).child(Self::detail_row("Ctrl + E","导出进程 CSV",cx)).child(Self::detail_row("Ctrl + P","暂停 / 恢复显示",cx))
            .child(h_flex().gap_6().child(div().w(px(150.)).child("窗口置顶")).child(Button::new("topmost").label(if self.settings.always_on_top {"已开启 · 点击关闭"} else {"开启置顶"}).on_click(cx.listener(|this,_,window,cx| {
                let next = !this.settings.always_on_top;
                match Self::apply_topmost(window, next) {
                    Ok(()) => { this.settings.always_on_top = next; let _ = this.settings_writer.send(this.settings.clone()); },
                    Err(error) => this.message = Some(error.to_string()),
                }cx.notify();
            }))))
            .child(h_flex().gap_6().child(div().w(px(150.)).child("访问权限")).child(Button::new("elevate").label(if self.elevated {"已是管理员"} else {"以管理员身份重新启动"}).disabled(self.elevated || self.operation_busy).on_click(cx.listener(|this,_,_,cx| this.restart_as_admin(cx)))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("逐进程网络跟踪、其他用户的进程及部分管理操作需要管理员权限。重新启动时由 Windows 请求权限确认。"))
            .child(self.update_panel(cx))
            .child(div().pt_5().text_lg().font_weight(FontWeight::SEMIBOLD).child("关于 SystemMonitorX")).child(div().text_color(cx.theme().muted_foreground).child(format!("v{} · 原生 Rust · Longbridge GPUI Kit 0.7.1 · Windows 10 1803+ / 11 x64", env!("CARGO_PKG_VERSION"))))
            .child(div().text_color(cx.theme().muted_foreground).child("数据来自 Windows 原生进程接口、DXGI、PDH、ETW、WMI 和 NVML。GPU 使用率显示最忙引擎；DirectX 12 能力按显卡实际查询。驱动或权限未提供的字段显示 —。"))
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
                                    Page::Processes
                                        | Page::Details
                                        | Page::Services
                                        | Page::Startup
                                        | Page::Users
                                        | Page::History
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
                        matches!(
                            self.page,
                            Page::Processes | Page::Details | Page::Performance
                        ),
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
                    .when(
                        matches!(
                            self.update_state.phase,
                            UpdatePhase::Available | UpdatePhase::Ready(_)
                        ),
                        |e| {
                            e.child(
                                h_flex()
                                    .px_6()
                                    .py_2()
                                    .gap_4()
                                    .bg(cx.theme().secondary)
                                    .child(div().flex_1().child(format!(
                                            "SystemMonitorX v{} 可更新",
                                            self.update_state
                                                .release
                                                .as_ref()
                                                .map(|r| r.version.as_str())
                                                .unwrap_or("—")
                                        )))
                                    .child(
                                        Button::new("show-update")
                                            .small()
                                            .label("查看更新")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.set_page(Page::Settings, window, cx)
                                            })),
                                    ),
                            )
                        },
                    )
                    .child(
                        div().flex_1().min_h_0().child(match self.page {
                            Page::Processes | Page::Details => self.process_view(cx),
                            Page::Users => self.users_view(cx),
                            Page::History => self.app_history_view(cx),
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
                                        .child(self.management_toolbar(cx))
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
                                            "{} · 采集 {:.1} ms · 运行时间 {}",
                                            s.host_name,
                                            s.collector_duration_ms,
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
            let testing = cfg!(feature = "ui-test")
                && std::env::args().any(|arg| {
                    matches!(arg.as_str(), "--ui-smoke-dir" | "--ui-update-smoke-version")
                });
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
            if let Some(expected) = std::env::args()
                .skip_while(|arg| arg != "--ui-update-smoke-version")
                .nth(1)
            {
                let directory = std::env::args()
                    .skip_while(|arg| arg != "--ui-update-smoke-report")
                    .nth(1)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::temp_dir().join(format!(
                            "SystemMonitorX-update-smoke-{}",
                            std::process::id()
                        ))
                    });
                qa_update::start(handle, view, expected, directory, cx);
            } else if let Some(directory) = std::env::args()
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
