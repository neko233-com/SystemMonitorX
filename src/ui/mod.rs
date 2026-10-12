mod columns;
mod developer_actions;
mod directx;
mod hardware;
mod localization;
mod management;
mod ports;
mod process_details;
mod process_icons;
#[cfg(feature = "ui-test")]
mod qa;
#[cfg(feature = "ui-test")]
mod qa_locales;
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
use localization::{UiMessage, diagnostic, mf, mlf, msg, network_diagnostic, tf, tr, trs};
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
use system_monitor_x::i18n::Language;
use system_monitor_x::{
    export::{write_process_csv, write_process_csv_rows},
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
    Ports,
    Performance,
    History,
    Users,
    Devices,
    Services,
    Startup,
    Settings,
}
impl Page {
    fn source_title(self) -> &'static str {
        match self {
            Self::Processes => "进程",
            Self::Details => "详细信息",
            Self::Ports => "端口与连接",
            Self::Performance => "性能",
            Self::History => "应用历史",
            Self::Users => "用户",
            Self::Devices => "设备与传感器",
            Self::Services => "服务",
            Self::Startup => "启动应用",
            Self::Settings => "设置",
        }
    }
    fn title(self) -> SharedString {
        tr(self.source_title())
    }
    fn search_source(self) -> &'static str {
        match self {
            Self::Ports => "搜索端口、协议、进程名称或 PID",
            Self::Services => "搜索服务名称、状态或 PID",
            Self::Startup => "搜索启动项名称、命令或来源",
            Self::Users => "搜索用户、会话 ID 或状态",
            Self::History => "搜索应用名称或路径",
            _ => "搜索进程名称、路径或 PID",
        }
    }
    fn id(self) -> &'static str {
        match self {
            Self::Processes => "nav-processes",
            Self::Details => "nav-details",
            Self::Ports => "nav-ports",
            Self::Performance => "nav-performance",
            Self::History => "nav-history",
            Self::Users => "nav-users",
            Self::Devices => "nav-devices",
            Self::Services => "nav-services",
            Self::Startup => "nav-startup",
            Self::Settings => "nav-settings",
        }
    }
    fn icon(self) -> UiIcon {
        match self {
            Self::Processes => UiIcon::Layers,
            Self::Details => UiIcon::Layers,
            Self::Ports => UiIcon::Network,
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
    group_processes: bool,
    language: Language,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            dark: true,
            auto_updates: true,
            always_on_top: false,
            hidden_process_columns: Vec::new(),
            group_processes: true,
            language: Language::System,
        }
    }
}
impl Settings {
    fn path() -> PathBuf {
        #[cfg(feature = "ui-test")]
        if let Some(directory) = std::env::args()
            .skip_while(|arg| arg != "--ui-smoke-dir")
            .nth(1)
        {
            return PathBuf::from(directory).join("settings.json");
        }
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
    process_display_seq: Option<u64>,
    users_display_seq: Option<u64>,
    history_display_seq: Option<u64>,
    stopped: Arc<AtomicBool>,
    interval: Arc<AtomicU64>,
    paused: bool,
    settings: Settings,
    resolved_language: Language,
    search: Entity<InputState>,
    task_input: Entity<InputState>,
    processes: Entity<TableState<ProcessTable>>,
    services: Entity<TableState<TextTable>>,
    startup: Entity<TableState<TextTable>>,
    users: Entity<TableState<TextTable>>,
    app_history: Entity<TableState<TextTable>>,
    ports: Entity<TableState<ports::PortsTable>>,
    ports_snapshot: Option<Arc<system_monitor_x::network_ports::PortSnapshot>>,
    ports_loading: bool,
    ports_last_refresh: Option<std::time::Instant>,
    service_entries: Vec<platform::ServiceInfo>,
    startup_entries: Vec<platform::StartupEntry>,
    history: VecDeque<HistoryPoint>,
    sample_index: u64,
    message: Option<UiMessage>,
    elevated: bool,
    metadata_loading: bool,
    updater: UpdateService,
    update_state: UpdateSnapshot,
    last_install_result: Option<system_monitor_x::update_install::InstallResult>,
    operation_busy: bool,
    install_after_download: bool,
    hardware: Option<Arc<system_monitor_x::hardware::HardwareInventory>>,
    hardware_sections: Vec<hardware::HardwareSection>,
    hardware_display_generation: u64,
    hardware_loading: bool,
    hardware_error: Option<String>,
    directx_inventory: Option<Arc<system_monitor_x::directx::DirectXInventory>>,
    directx_loading: bool,
    directx_error: Option<String>,
    process_details: Option<Arc<ProcessDetails>>,
    process_details_loading: bool,
    process_details_error: Option<UiMessage>,
    process_details_identity: Option<(u32, u64)>,
    process_details_expanded: bool,
    process_details_generation: u64,
    file_signature: Option<Arc<system_monitor_x::file_signature::FileSignature>>,
    file_signature_loading: bool,
    file_signature_error: Option<UiMessage>,
    file_signature_generation: u64,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
    settings_writer: std::sync::mpsc::Sender<Settings>,
    settings_error: Arc<Mutex<Option<UiMessage>>>,
}
impl Drop for SystemMonitorApp {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}
impl SystemMonitorApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();
        let resolved_language = system_monitor_x::i18n::set_language(settings.language);
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
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("搜索进程名称、路径或 PID")));
        let task_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr("例如 notepad.exe 或可执行文件路径"))
        });
        let process_action_parent = cx.entity().downgrade();
        let processes = cx.new(|cx| {
            let mut delegate = ProcessTable::new();
            delegate.set_hidden_columns(&settings.hidden_process_columns);
            delegate.group_mode = settings.group_processes;
            delegate.set_action_handler(move |request, window, cx| {
                let _ = process_action_parent.update(cx, |this: &mut SystemMonitorApp, cx| {
                    this.handle_process_action(request, window, cx)
                });
            });
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
                .numeric_columns(&[3])
                .localized_columns(&[2, 4]),
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
                ])
                .stable_identity_columns(&[4])
                .localized_columns(&[1, 3]),
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
                .numeric_columns(&[1, 3, 4, 5, 6])
                .stable_identity_columns(&[1])
                .localized_columns(&[2]),
                window,
                cx,
            )
            .col_selectable(false)
            .col_movable(false)
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
                .numeric_columns(&[1, 2, 3, 4, 5, 6])
                .stable_identity_columns(&[8]),
                window,
                cx,
            )
            .col_selectable(false)
            .col_movable(false)
        });
        let ports = cx.new(|cx| {
            TableState::new(ports::PortsTable::new(), window, cx)
                .col_selectable(false)
                .col_movable(false)
        });
        let subscriptions = vec![
            cx.subscribe(&ports, |_, table, event: &TableEvent, cx| match event {
                TableEvent::SelectRow(row) | TableEvent::DoubleClickedRow(row) => {
                    if table.read(cx).selected_row() == Some(*row) {
                        table.update(cx, |table, _| table.delegate_mut().select_row(Some(*row)));
                    }
                }
                TableEvent::ClearSelection => {
                    if table.read(cx).selected_row().is_none() {
                        table.update(cx, |table, _| table.delegate_mut().select_row(None));
                    }
                }
                TableEvent::ColumnWidthsChanged(widths) => {
                    table.update(cx, |table, _| {
                        table.delegate_mut().set_column_widths(widths)
                    });
                }
                _ => {}
            }),
            cx.subscribe_in(
                &ports,
                window,
                |this, _, event: &ports::PortMenuEvent, window, cx| {
                    this.handle_port_action(event.clone(), window, cx);
                },
            ),
            cx.subscribe_in(&search, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filter(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&processes, |this, table, event: &TableEvent, cx| {
                match event {
                    TableEvent::SelectRow(i) | TableEvent::DoubleClickedRow(i) => {
                        if table.read(cx).selected_row() != Some(*i) {
                            return;
                        }
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
                        if table.read(cx).selected_row().is_some() {
                            return;
                        }
                        table.update(cx, |t, _| t.delegate_mut().selected = None);
                        this.process_details_identity = None;
                        this.process_details = None;
                        this.process_details_error = None;
                        this.process_details_loading = false;
                        this.file_signature = None;
                        this.file_signature_loading = false;
                        this.file_signature_error = None;
                        this.file_signature_generation =
                            this.file_signature_generation.wrapping_add(1);
                    }
                    _ => {}
                }
                cx.notify();
            }),
        ];
        let mut subscriptions = subscriptions;
        for text_table in [&services, &startup, &users, &app_history] {
            subscriptions.push(
                cx.subscribe(text_table, |_, table, event: &TableEvent, cx| match event {
                    TableEvent::SelectRow(row) | TableEvent::DoubleClickedRow(row) => {
                        if table.read(cx).selected_row() == Some(*row) {
                            table
                                .update(cx, |table, _| table.delegate_mut().select_row(Some(*row)));
                        }
                    }
                    TableEvent::ClearSelection => {
                        if table.read(cx).selected_row().is_none() {
                            table.update(cx, |table, _| table.delegate_mut().select_row(None));
                        }
                    }
                    TableEvent::ColumnWidthsChanged(widths) => {
                        table.update(cx, |table, _| {
                            table.delegate_mut().set_column_widths(widths)
                        });
                    }
                    _ => {}
                }),
            );
        }
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
                        *slot = Some(mf(
                            "设置保存失败：{error}",
                            &[("error", format!("{error}"))],
                        ));
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
            process_display_seq: None,
            users_display_seq: None,
            history_display_seq: None,
            stopped,
            interval,
            paused: false,
            settings,
            resolved_language,
            search,
            task_input,
            processes,
            services,
            startup,
            users,
            app_history,
            ports,
            ports_snapshot: None,
            ports_loading: false,
            ports_last_refresh: None,
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
            hardware_display_generation: 0,
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
            file_signature: None,
            file_signature_loading: false,
            file_signature_error: None,
            file_signature_generation: 0,
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
                    Err(error) => this.message = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
        if app.settings.always_on_top
            && let Err(error) = Self::apply_topmost(window, true)
        {
            app.message = Some(error.to_string().into());
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
        if matches!(self.page, Page::Processes | Page::Details)
            && self.processes.update(cx, |table, cx| {
                let changed = table.delegate_mut().poll_icon_updates();
                if changed {
                    cx.notify();
                }
                changed
            })
        {
            cx.notify();
        }
        if self.paused {
            return;
        }
        if self.page == Page::Ports
            && !self.ports_loading
            && self
                .ports_last_refresh
                .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(2))
        {
            self.refresh_ports(cx);
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
        self.snapshot = Some(snapshot);
        self.refresh_visible_snapshot(cx);
        cx.notify();
    }
    /// Only the visible page builds its presentation model. The collector and
    /// chart/history sampling continue independently; navigation catches up to
    /// the latest immutable snapshot, including while display is paused.
    fn refresh_visible_snapshot(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let seq = self.last_seq;
        if matches!(self.page, Page::Processes | Page::Details)
            && self.process_display_seq != Some(seq)
        {
            self.processes.update(cx, |table, cx| {
                let d = table.delegate_mut();
                d.set_snapshot(snapshot.clone());
                tables::sync_selection(table, cx);
                cx.notify();
            });
            self.process_display_seq = Some(seq);
        }
        if self.page == Page::Users && self.users_display_seq != Some(seq) {
            self.users.update(cx, |table, cx| {
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
                                s.client_name.clone().unwrap_or_else(|| trs("本机")),
                                session_protocol(s.protocol.as_deref())
                            ),
                        ]
                    })
                    .collect();
                d.rebuild();
                tables::sync_text_selection(table, cx);
                cx.notify();
            });
            self.users_display_seq = Some(seq);
        }
        if self.page == Page::History && self.history_display_seq != Some(seq) {
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
                tables::sync_text_selection(table, cx);
                cx.notify();
            });
            self.history_display_seq = Some(seq);
        }
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
        if self.page == Page::Ports {
            self.ports.update(cx, |table, cx| {
                table.delegate_mut().set_query(&query);
                table.scroll_to_row(0, cx);
                table.clear_selection(cx);
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
                tables::sync_text_selection(t, cx);
                t.scroll_to_row(0, cx);
                cx.notify();
            });
        }
    }
    fn set_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        self.search.update(cx, |input, cx| {
            input.set_placeholder(tr(page.search_source()), window, cx)
        });
        if matches!(page, Page::Processes | Page::Details) {
            self.processes.update(cx, |table, cx| {
                let detailed = page == Page::Details;
                if table.delegate().detailed != detailed {
                    table.delegate_mut().detailed = detailed;
                    table.delegate_mut().rebuild();
                    tables::sync_selection(table, cx);
                }
                table.refresh(cx);
            });
        }
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        // Programmatic InputState::set_value does not emit InputEvent::Change.
        // Reset the newly visible model; hidden pages are not searched on each key.
        self.filter(cx);
        self.refresh_visible_snapshot(cx);
        if page == Page::Ports {
            self.refresh_ports(cx);
        }
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
                            tables::sync_text_selection(t, cx);
                            cx.notify();
                        });
                    }
                    Err(e) => {
                        this.message = Some(mf("服务读取失败：{e:#}", &[("e", format!("{e:#}"))]))
                    }
                }
                match results.1 {
                    Ok(rows) => {
                        this.startup_entries = rows.clone();
                        this.startup.update(cx, |t, cx| {
                            t.delegate_mut().all = rows
                                .into_iter()
                                .map(|s| {
                                    let identity = s.selection_key();
                                    vec![
                                        s.name,
                                        s.enabled
                                            .map(|v| if v { "已启用" } else { "已禁用" }.to_owned())
                                            .unwrap_or_else(|| "不可用".to_owned()),
                                        s.command,
                                        s.source,
                                        identity,
                                    ]
                                })
                                .collect();
                            t.delegate_mut().rebuild();
                            tables::sync_text_selection(t, cx);
                            cx.notify();
                        });
                    }
                    Err(e) => {
                        this.message = Some(mf("启动项读取失败：{e:#}", &[("e", format!("{e:#}"))]))
                    }
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
        operation: impl FnOnce() -> anyhow::Result<UiMessage> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.operation_busy {
            self.message = Some(msg("有一项操作正在执行，请稍候"));
            cx.notify();
            return;
        }
        self.operation_busy = true;
        self.message = Some(UiMessage::Progress(label.to_owned()));
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = smol::unblock(operation).await;
            let _ = view.update(cx, |this, cx| {
                this.operation_busy = false;
                this.message = Some(match result {
                    Ok(message) => message,
                    Err(error) => mf("操作失败：{error:#}", &[("error", format!("{error:#}"))]),
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
        self.message = Some(msg("正在重新校验安装包…"));
        cx.spawn(async move |view, cx| {
            let result =
                smol::unblock(move || system_monitor_x::updater::launch_installer(&path, &release))
                    .await;
            let success = result.is_ok();
            let _ = view.update(cx, |this, cx| {
                this.operation_busy = false;
                if let Err(error) = result {
                    this.message = Some(mf(
                        "启动安装器失败：{error:#}",
                        &[("error", format!("{error:#}"))],
                    ));
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
                .title(tr("运行新任务"))
                .width(px(480.))
                .child(Input::new(&input))
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("task-cancel")
                                .label(tr("取消"))
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("task-run")
                                .primary()
                                .label(tr("运行"))
                                .on_click({
                                    let input = input.clone();
                                    let view = view.clone();
                                    move |_, w, cx| {
                                        if Self::submit_task(&input, &view, cx) {
                                            w.close_dialog(cx);
                                        }
                                    }
                                }),
                        ),
                )
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(tr("运行"))
                        .cancel_text(tr("取消"))
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
                this.message = Some(msg("启动失败：请输入任务命令"));
                cx.notify();
            });
            return false;
        }
        let _ = view.update(cx, |this, cx| {
            this.run_operation(
                "正在启动任务",
                move || {
                    platform::launch_task(&command)?;
                    Ok(msg("任务已启动"))
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
        let delegate = self.processes.read(cx).delegate();
        if let Some(reason) = delegate.selected_application_unavailable_reason() {
            self.message = Some(msg(&reason));
            cx.notify();
            return;
        }
        if let Some(targets) = delegate.selected_application_targets() {
            let count = targets.len();
            self.confirm_management(
                "结束应用",
                tf("结束 {} 的 {count} 个已列出进程？未保存的数据可能丢失。确认期间新建的进程不在本次范围内。", &[("arg0", p.name.to_string()), ("count", format!("{count}"))]),
                true,
                move || {
                    let ended = platform::terminate_application_processes(&targets)?;
                    Ok(mf("已结束应用的 {} 个进程", &[("arg0", format!("{}", ended.len()))]))
                },
                w,
                cx,
            );
            return;
        }
        let view = cx.entity().downgrade();
        w.open_alert_dialog(cx, move |dialog, _, _| {
            dialog
                .confirm()
                .title(tr("结束进程"))
                .description(tf(
                    "结束 {}（PID {}）？未保存的数据可能丢失。",
                    &[("arg0", p.name.to_string()), ("arg1", format!("{}", p.pid))],
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(tr("结束进程"))
                        .cancel_text(tr("取消"))
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
                                    Ok(msg("进程已结束"))
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
        if self.page == Page::Ports {
            self.export_ports(cx);
            return;
        }
        let selected = if matches!(self.page, Page::Processes | Page::Details) {
            self.processes.read(cx).delegate().export_snapshot()
        } else {
            self.snapshot.clone().map(|snapshot| (snapshot, Vec::new()))
        };
        let Some((snapshot, indices)) = selected else {
            return;
        };
        let filtered = matches!(self.page, Page::Processes | Page::Details);
        let directory =
            PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_else(|| ".".into()));
        let prompt = cx.prompt_for_new_path(&directory, Some("SystemMonitorX-processes.csv"));
        cx.spawn(async move |view, cx| {
            if let Ok(Ok(Some(path))) = prompt.await {
                let result = smol::unblock(move || {
                    if filtered {
                        write_process_csv_rows(&snapshot, &indices, &path)?;
                    } else {
                        write_process_csv(&snapshot, &path)?;
                    }
                    Ok::<PathBuf, anyhow::Error>(path)
                })
                .await;
                let _ = view.update(cx, |this, cx| {
                    this.message = Some(match result {
                        Ok(path) => mf("已导出 {}", &[("arg0", format!("{}", path.display()))]),
                        Err(e) => mf("导出失败：{e:#}", &[("e", format!("{e:#}"))]),
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
                    .child(tr("系统")),
            )
            .child(
                v_flex()
                    .id("sidebar-nav")
                    .test_support()
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
                            Page::Ports,
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
                    .id("sidebar-footer")
                    .test_support()
                    .flex_shrink_0()
                    .p_5()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(Icon::new(UiIcon::Shield).size(px(14.)).flex_shrink_0())
                            .child(
                                div()
                                    .id("sidebar-permission")
                                    .test_support()
                                    .flex_1()
                                    .min_w_0()
                                    .child(tr(if self.elevated {
                                        "管理员权限"
                                    } else {
                                        "标准用户权限"
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Rust · GPUI Kit 0.7.3"),
                    ),
            )
            .into_any_element()
    }
    fn nav_item(&self, page: Page, cx: &mut Context<Self>) -> AnyElement {
        let active = self.page == page;
        div()
            .id(page.id())
            .mx_2()
            .my_1()
            .px_4()
            .min_h(px(42.))
            .py_2()
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
            .child(div().flex_1().min_w_0().child(page.title()))
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
                                    .child(tr(name)),
                            )
                            .child(div().font_family("Consolas").text_2xl().child(value))
                    }),
            )
            .into_any_element()
    }
    fn process_view(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let table = self.processes.read(cx).delegate();
        let p = table.selection().cloned();
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .id("process-actions")
                    .test_support()
                    .flex_shrink_0()
                    .min_h(px(50.))
                    .py_2()
                    .flex_wrap()
                    .gap_2()
                    .px_6()
                    .justify_between()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(tf(
                                "{} 个进程 · {} 行",
                                &[
                                    (
                                        "arg0",
                                        format!(
                                            "{}",
                                            self.snapshot
                                                .as_ref()
                                                .map(|s| s.processes.len())
                                                .unwrap_or(table.rows.len())
                                        ),
                                    ),
                                    ("arg1", format!("{}", table.rows.len())),
                                ],
                            )),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .justify_end()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new("process-columns")
                                    .ghost()
                                    .small()
                                    .label(tr("选择列"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.columns_dialog(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("application-groups")
                                    .ghost()
                                    .small()
                                    .label(tr(
                                        if self.processes.read(cx).delegate().group_mode
                                            && !self.processes.read(cx).delegate().tree_mode
                                        {
                                            "应用分组 ✓"
                                        } else {
                                            "应用分组"
                                        },
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let enabled = this.processes.update(cx, |table, cx| {
                                            let delegate = table.delegate_mut();
                                            delegate.group_mode =
                                                !delegate.group_mode || delegate.tree_mode;
                                            delegate.tree_mode = false;
                                            let enabled = delegate.group_mode;
                                            delegate.rebuild();
                                            tables::sync_selection(table, cx);
                                            cx.notify();
                                            enabled
                                        });
                                        this.settings.group_processes = enabled;
                                        let _ = this.settings_writer.send(this.settings.clone());
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("tree-mode")
                                    .ghost()
                                    .small()
                                    .label(tr(if table.tree_mode {
                                        "退出进程树"
                                    } else {
                                        "进程树"
                                    }))
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
                                    .label(tr("导出 CSV"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.export_csv(&ExportCsv, w, cx)
                                    })),
                            )
                            .child(
                                Button::new("end")
                                    .small()
                                    .danger()
                                    .icon(UiIcon::Square)
                                    .label(tr("结束任务"))
                                    .disabled(p.is_none())
                                    .on_click(cx.listener(|this, _, w, cx| this.end_task(w, cx))),
                            ),
                    ),
            )
            .when_some(self.snapshot.as_ref(), |element, snapshot| {
                let notes: Vec<_> = snapshot
                    .gpu_note
                    .iter()
                    .map(|note| diagnostic(note))
                    .chain(
                        (!snapshot.process_network_available)
                            .then(|| network_diagnostic(&snapshot.process_network_status)),
                    )
                    .collect();
                element.when(!notes.is_empty(), |element| {
                    element.child(
                        v_flex()
                            .id("process-diagnostics-scroll")
                            .test_support()
                            .flex_shrink_0()
                            .max_h(px(64.))
                            .overflow_y_scroll()
                            .px_6()
                            .py_1()
                            .gap_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .children(notes.into_iter().map(|note| div().child(note))),
                    )
                })
            })
            .child(
                div()
                    .id("process-table-region")
                    .test_support()
                    .flex_1()
                    .min_h(px(120.))
                    .child(
                        DataTable::new(&self.processes)
                            .bordered(false)
                            .stripe(false)
                            .small(),
                    ),
            )
            .when_some(p, |e, p| {
                e.child(self.process_inspector(
                    p,
                    px((window.viewport_size().height.as_f32() * 0.25).clamp(120., 300.)),
                    cx,
                ))
            })
            .into_any_element()
    }
    fn process_inspector(
        &self,
        p: system_monitor_x::monitor::ProcessSample,
        maximum_height: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = p.exe.as_ref().map(|p| p.to_string_lossy().to_string());
        v_flex()
            .flex_shrink_0()
            .id("process-inspector-scroll")
            .test_support()
            .max_h(maximum_height)
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
                            .child(
                                div().text_color(cx.theme().muted_foreground).child(tf(
                                    "PID {} · 已运行 {}",
                                    &[
                                        ("arg0", format!("{}", p.pid)),
                                        (
                                            "arg1",
                                            (if p.start_time_unix_secs > 0 {
                                                duration(p.run_time_secs)
                                            } else {
                                                trs("不可用")
                                            })
                                            .to_string(),
                                        ),
                                    ],
                                )),
                            ),
                    )
                    .child(
                        Button::new("location")
                            .small()
                            .ghost()
                            .icon(UiIcon::FolderOpen)
                            .label(tr("打开文件位置"))
                            .disabled(path.is_none())
                            .on_click(cx.listener({
                                let path = path.clone();
                                move |this, _, _, cx| {
                                    if let Some(path) = path.clone() {
                                        this.run_operation(
                                            "正在打开文件位置",
                                            move || {
                                                platform::open_file_location(&path)?;
                                                Ok(msg("文件位置已打开"))
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
                    .child(path.unwrap_or_else(|| trs("系统或受保护进程：路径不可访问"))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr("进程优先级")),
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
                                .label(tr(name))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.run_operation(
                                        "正在设置优先级",
                                        move || {
                                            platform::set_priority(
                                                p.pid,
                                                p.start_time_windows_ticks.unwrap_or(0),
                                                priority,
                                            )?;
                                            Ok(mlf("优先级已设为{name}", &[("name", msg(name))]))
                                        },
                                        cx,
                                    );
                                }))
                        }),
                    ),
            )
            .when_some(
                self.processes.read(cx).delegate().selected_group_note(),
                |e, note| {
                    e.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(diagnostic(note)),
                    )
                },
            )
            .child(self.extra_process_controls(p, cx))
            .child(self.selected_process_details(cx))
            .into_any_element()
    }
    fn chart(
        &self,
        title: &'static str,
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
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(tr(title)))
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
                            .child(tr("等待有效采样"))
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("最近 120 次采样")),
            )
            .into_any_element()
    }
    fn performance(&self, cx: &Context<Self>) -> AnyElement {
        let Some(s) = self.snapshot.as_deref() else {
            return div()
                .p_8()
                .child(tr("正在采集系统信息…"))
                .into_any_element();
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
                    .child(tr("系统内存与对象")),
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
                .map(|(label, value)| Self::detail_row(tr(label), value, cx)),
            )
            .when_some(s.process_collection_note.clone(), |e, note| {
                e.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(diagnostic(&note)),
                )
            })
            .child(
                div()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_5()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("逻辑处理器")),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tf(
                        "{} · {} 逻辑处理器 · {} · {}",
                        &[
                            ("arg0", s.cpu_name.to_string()),
                            ("arg1", format!("{}", s.logical_cpus)),
                            (
                                "arg2",
                                s.physical_cores
                                    .map(|n| tf("{n} 物理核心", &[("n", n.to_string())]))
                                    .unwrap_or_else(|| trs("物理核心数不可用"))
                                    .to_string(),
                            ),
                            (
                                "arg3",
                                s.cpu_frequency_mhz
                                    .map(|n| format!("{:.2} GHz", n as f64 / 1000.))
                                    .unwrap_or_else(|| trs("频率不可用"))
                                    .to_string(),
                            ),
                        ],
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
            return div().p_8().child(tr("正在读取设备…")).into_any_element();
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
                            .child(tr("系统信息")),
                    )
                    .child(Self::detail_row(tr("计算机"), s.host_name.clone(), cx))
                    .child(Self::detail_row(tr("操作系统"), s.os_version.clone(), cx))
                    .child(Self::detail_row(tr("处理器"), s.cpu_name.clone(), cx))
                    .child(Self::detail_row(
                        tr("物理内存"),
                        tf(
                            "已用 {} / 总计 {} · 可用 {}",
                            &[
                                ("arg0", bytes(s.memory_used).to_string()),
                                ("arg1", bytes(s.memory_total).to_string()),
                                ("arg2", bytes(s.memory_available).to_string()),
                            ],
                        ),
                        cx,
                    ))
                    .child(Self::detail_row(
                        tr("交换空间"),
                        format!("{} / {}", bytes(s.swap_used), bytes(s.swap_total)),
                        cx,
                    ))
                    .child(Self::detail_row(
                        tr("运行时间"),
                        duration(s.uptime_secs),
                        cx,
                    )),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr("图形处理器")),
                    )
                    .children(s.gpus.iter().map(|g| {
                        v_flex()
                            .child(Self::detail_row(
                                g.name.clone(),
                                tf(
                                    "GPU {} · {} · 专用显存 {} / {} · 共享显存 {}",
                                    &[
                                        ("arg0", format!("{}", g.index)),
                                        ("arg1", percent(g.usage_percent).to_string()),
                                        (
                                            "arg2",
                                            g.dedicated_memory_used
                                                .map(bytes)
                                                .unwrap_or_else(|| trs("不可用"))
                                                .to_string(),
                                        ),
                                        ("arg3", bytes(g.dedicated_memory_total).to_string()),
                                        (
                                            "arg4",
                                            g.shared_memory_used
                                                .map(bytes)
                                                .unwrap_or_else(|| trs("不可用"))
                                                .to_string(),
                                        ),
                                    ],
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
                                    .child(diagnostic(&g.status)),
                            )
                            .child(Self::detail_row(
                                tr("温度 / 风扇"),
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
                                tr("功耗 / 上限"),
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
                                tr("核心 / 显存频率"),
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
                                tr("PCI / 驱动"),
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
                                tr("VBIOS / 传感器来源"),
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
                                        .child(diagnostic(&note)),
                                )
                            })
                    }))
                    .when_some(s.gpu_note.clone(), |e, n| {
                        e.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(diagnostic(&n)),
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
                            .child(tr("存储设备")),
                    )
                    .children(s.disks.iter().map(|d| {
                        Self::detail_row(
                            d.mount_point.clone(),
                            tf(
                                "{} · {} · 可用 {} / {} · 读 {} · 写 {}",
                                &[
                                    ("arg0", d.name.to_string()),
                                    ("arg1", d.kind.to_string()),
                                    ("arg2", bytes(d.available_bytes).to_string()),
                                    ("arg3", bytes(d.total_bytes).to_string()),
                                    ("arg4", rate(d.read_bytes_per_sec).to_string()),
                                    ("arg5", rate(d.write_bytes_per_sec).to_string()),
                                ],
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
                            .child(tr("网络接口")),
                    )
                    .children(s.networks.iter().map(|n| {
                        Self::detail_row(
                            n.name.clone(),
                            tf(
                                "{} · {} · 接收 {} · 发送 {} · 累计接收 {}",
                                &[
                                    ("arg0", n.state.to_string()),
                                    ("arg1", n.mac_address.to_string()),
                                    ("arg2", rate(n.received_bytes_per_sec).to_string()),
                                    ("arg3", rate(n.transmitted_bytes_per_sec).to_string()),
                                    ("arg4", bytes(n.total_received_bytes).to_string()),
                                ],
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
                            .child(tr("温度传感器")),
                    )
                    .children(s.temperatures.iter().map(|t| {
                        Self::detail_row(
                            t.label.clone(),
                            t.celsius
                                .map(|v| format!("{v:.1} °C"))
                                .unwrap_or_else(|| trs("不可用")),
                            cx,
                        )
                    }))
                    .when_some(s.temperature_note.clone(), |e, n| {
                        e.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(diagnostic(&n)),
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
            UpdatePhase::Idle => trs("等待后台检查"),
            UpdatePhase::Checking => trs("正在后台检查更新…"),
            UpdatePhase::Current => trs("已是最新稳定版本"),
            UpdatePhase::Available => tf(
                "发现新版本 v{}",
                &[(
                    "arg0",
                    self.update_state
                        .release
                        .as_ref()
                        .map(|r| r.version.as_str())
                        .unwrap_or("—")
                        .to_string(),
                )],
            ),
            UpdatePhase::Downloading { received, total } => tf(
                "正在下载：{} / {}",
                &[
                    ("arg0", bytes(*received).to_string()),
                    ("arg1", bytes(*total).to_string()),
                ],
            ),
            UpdatePhase::Ready(_) => trs("安装包已通过 SHA-256 校验，可以安装"),
            UpdatePhase::Failed(error) => tf(
                "更新检查/下载暂不可用：{error}",
                &[("error", error.to_string())],
            ),
        };
        let newer = self.update_state.release.as_ref().is_some_and(|r| {
            system_monitor_x::updater::is_newer(&r.version, env!("CARGO_PKG_VERSION"))
                .unwrap_or(false)
        });
        v_flex().gap_3()
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(tr("应用更新")))
            .child(h_flex().gap_3()
                .child(Button::new("auto-updates").label(tr(if self.settings.auto_updates { "后台自动检查：开启" } else { "后台自动检查：关闭" })).on_click(cx.listener(|this, _, _, cx| {
                    this.settings.auto_updates = !this.settings.auto_updates;
                    this.updater.set_enabled(this.settings.auto_updates);
                    this.update_state.enabled = this.settings.auto_updates;
                    let _ = this.settings_writer.send(this.settings.clone());
                    cx.notify();
                })))
                .child(Button::new("check-update").label(tr("立即检查")).disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                    this.updater.check();
                    this.update_state.phase = UpdatePhase::Checking;
                    cx.notify();
                }))))
            .child(div().text_sm().child(status))
            .when_some(self.update_state.checked_at, |e, checked| {
                let ago = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs().saturating_sub(checked);
                e.child(div().text_xs().text_color(cx.theme().muted_foreground).child(tf("上次成功检查：{} 秒前", &[("arg0", format!("{}", ago))])))
            })
            .when(newer && !matches!(self.update_state.phase, UpdatePhase::Ready(_)), |e| {
                e.child(Button::new("download-update").primary().label(tr("一键更新")).disabled(busy || self.operation_busy).on_click(cx.listener(|this, _, _, cx| {
                    this.install_after_download = true;
                    this.updater.download();
                    this.update_state.phase = UpdatePhase::Downloading { received: 0, total: this.update_state.release.as_ref().map(|r| r.installer_bytes).unwrap_or(0) };
                    cx.notify();
                })))
            })
            .when(matches!(self.update_state.phase, UpdatePhase::Ready(_)), |e|
                e.child(Button::new("install-update").primary().label(tr("一键更新 · 安装已校验版本")).disabled(self.operation_busy).on_click(cx.listener(|this, _, _, cx| this.install_update(cx)))))
            .when_some(self.last_install_result.clone(), |e, result| {
                let log = result.log_path.to_string_lossy().into_owned();
                let mut details = v_flex().gap_1()
                    .child(div().text_sm().child(tf("上次安装 v{}：{}", &[("arg0", result.version.to_string()), ("arg1", result.message.to_string())])));
                if let Some(error) = result.restart_error {
                    details = details.child(div().text_xs().child(tf("重新启动失败：{error}", &[("error", error.to_string())])));
                }
                e.child(details.child(Button::new("install-log").ghost().small().label(tr("查看安装日志位置"))
                    .disabled(self.operation_busy).on_click(cx.listener(move |this, _, _, cx| {
                        let log = log.clone();
                        this.run_operation("正在打开安装日志", move || {
                            platform::open_file_location(&log)?;
                            Ok(msg("安装日志位置已打开"))
                        }, cx);
                    }))))
            })
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(tr("应用运行时每 6 小时后台检查稳定版；网络失败后自动重试。点击“一键更新”后自动下载、校验，由独立安装助手完成升级并重新启动。便携版也会安装到本用户的应用目录。")))
            .into_any_element()
    }
    fn apply_topmost(window: &Window, enabled: bool) -> anyhow::Result<()> {
        let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)
            .map_err(|e| {
                anyhow::anyhow!(tf(
                    "无法读取原生窗口句柄：{e:?}",
                    &[("e", format!("{e:?}"))]
                ))
            })?
            .as_raw()
        else {
            anyhow::bail!(trs("当前窗口不是 Windows 原生窗口"));
        };
        system_monitor_x::runtime::set_window_topmost(handle.hwnd.get(), enabled)
    }
    fn restart_as_admin(&mut self, cx: &mut Context<Self>) {
        if self.operation_busy || self.elevated {
            return;
        }
        self.operation_busy = true;
        self.message = Some(msg("等待 Windows 管理员权限确认…"));
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = smol::unblock(system_monitor_x::runtime::relaunch_elevated).await;
            let success = result.is_ok();
            let _ = view.update(cx, |this, cx| {
                this.operation_busy = false;
                if let Err(error) = result {
                    this.message = Some(format!("{error:#}").into());
                }
                cx.notify();
            });
            if success {
                cx.update(|cx| cx.quit());
            }
        })
        .detach();
    }
    fn set_language(&mut self, language: Language, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.language = language;
        self.resolved_language = system_monitor_x::i18n::set_language(language);
        self.search.update(cx, |input, cx| {
            input.set_placeholder(tr(self.page.search_source()), window, cx)
        });
        self.task_input.update(cx, |input, cx| {
            input.set_placeholder(tr("例如 notepad.exe 或可执行文件路径"), window, cx)
        });
        // Refresh cached headers only; the native identities, query, ordering,
        // row selection, column widths and hidden-column state stay in place.
        self.processes.update(cx, |table, cx| {
            table.refresh(cx);
            tables::sync_selection(table, cx);
            cx.notify();
        });
        self.ports.update(cx, |table, cx| {
            table.refresh(cx);
            ports::sync_selection(table, cx);
            cx.notify();
        });
        for table in [
            &self.services,
            &self.startup,
            &self.users,
            &self.app_history,
        ] {
            table.update(cx, |table, cx| {
                table.delegate_mut().rebuild();
                table.refresh(cx);
                tables::sync_text_selection(table, cx);
                cx.notify();
            });
        }
        // Localized session client/protocol fallbacks derive from the cached snapshot.
        self.users_display_seq = None;
        self.refresh_visible_snapshot(cx);
        self.rebuild_hardware_presentation(cx);
        let _ = self.settings_writer.send(self.settings.clone());
        cx.notify();
        window.refresh();
    }
    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .id("page-toolbar")
            .flex_shrink_0()
            .px_6()
            .py_3()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_3()
                    .min_w_0()
                    .child(
                        div()
                            .id("page-title")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.page.title()),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_2()
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
                                    .label(tr(if self.paused { "恢复" } else { "暂停" }))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_pause(&TogglePause, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("new-task")
                                    .small()
                                    .icon(UiIcon::Plus)
                                    .label(tr("运行新任务"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.new_task(&NewTask, window, cx)
                                    })),
                            ),
                    ),
            )
            .when(
                matches!(
                    self.page,
                    Page::Processes
                        | Page::Details
                        | Page::Services
                        | Page::Startup
                        | Page::Users
                        | Page::History
                        | Page::Ports
                ),
                |element| {
                    element.child(
                        div().w_full().child(
                            Input::new(&self.search)
                                .prefix(Icon::new(UiIcon::Search).size(px(16.)))
                                .small(),
                        ),
                    )
                },
            )
            .into_any_element()
    }
    fn language_selector(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .id("language-section")
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("界面语言")),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .children(Language::ALL.into_iter().map(|language| {
                        Button::new(format!("language-{}", language.tag()))
                            .small()
                            .label(if language == Language::System {
                                tr("跟随系统")
                            } else {
                                language.native_name().into()
                            })
                            .when(self.settings.language == language, |button| {
                                button.primary()
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_language(language, window, cx)
                            }))
                    })),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(tr("当前界面语言"))
                    .child(self.resolved_language.native_name()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("语言选择会自动保存，并立即应用到所有页面。")),
            )
            .into_any_element()
    }
    fn settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex().id("settings-scroll").size_full().overflow_y_scroll().p_8().gap_6().child(self.language_selector(cx)).child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(tr("采样与外观")))
            .child(h_flex().gap_6().child(div().w(px(150.)).child(tr("刷新间隔"))).children([(500,"0.5 秒"),(1000,"1 秒"),(2000,"2 秒")].into_iter().map(|(ms,label)|Button::new(format!("interval-{ms}")).label(tr(label)).when(self.settings.interval_ms==ms,|b|b.primary()).on_click(cx.listener(move|this,_,_,cx|{this.settings.interval_ms=ms;this.interval.store(ms,Ordering::Relaxed);let _=this.settings_writer.send(this.settings.clone());cx.notify();})))))
            .child(h_flex().gap_6().child(div().w(px(150.)).child(tr("主题"))).child(Button::new("theme").label(tr(if self.settings.dark {"切换为浅色"}else{"切换为深色"})).on_click(cx.listener(|this,_,w,cx|{this.settings.dark = !this.settings.dark;Theme::change(if this.settings.dark{ThemeMode::Dark}else{ThemeMode::Light},Some(w),cx);let _=this.settings_writer.send(this.settings.clone());cx.notify();}))))
            .child(div().pt_5().border_t_1().border_color(cx.theme().border).text_lg().font_weight(FontWeight::SEMIBOLD).child(tr("快捷键"))).child(Self::detail_row("Ctrl + F",trs("搜索"),cx)).child(Self::detail_row("Ctrl + N",trs("运行新任务"),cx)).child(Self::detail_row("Ctrl + E",trs("导出进程 CSV"),cx)).child(Self::detail_row("Ctrl + P",trs("暂停 / 恢复显示"),cx))
            .child(h_flex().gap_6().child(div().w(px(150.)).child(tr("窗口置顶"))).child(Button::new("topmost").label(tr(if self.settings.always_on_top {"已开启 · 点击关闭"} else {"开启置顶"})).on_click(cx.listener(|this,_,window,cx| {
                let next = !this.settings.always_on_top;
                match Self::apply_topmost(window, next) {
                    Ok(()) => { this.settings.always_on_top = next; let _ = this.settings_writer.send(this.settings.clone()); },
                    Err(error) => this.message = Some(error.to_string().into()),
                }cx.notify();
            }))))
            .child(h_flex().gap_6().child(div().w(px(150.)).child(tr("访问权限"))).child(Button::new("elevate").label(tr(if self.elevated {"已是管理员"} else {"以管理员身份重新启动"})).disabled(self.elevated || self.operation_busy).on_click(cx.listener(|this,_,_,cx| this.restart_as_admin(cx)))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(tr("逐进程网络跟踪、其他用户的进程及部分管理操作需要管理员权限。重新启动时由 Windows 请求权限确认。")))
            .child(self.update_panel(cx))
            .child(div().pt_5().text_lg().font_weight(FontWeight::SEMIBOLD).child(tr("关于 SystemMonitorX"))).child(div().text_color(cx.theme().muted_foreground).child(tf("v{} · 原生 Rust · Longbridge GPUI Kit 0.7.3 · Windows 10 1803+ / 11 x64", &[("arg0", env!("CARGO_PKG_VERSION").to_string())])))
            .child(div().text_color(cx.theme().muted_foreground).child(tr("数据来自 Windows 原生进程接口、DXGI、PDH、ETW、WMI 和 NVML。GPU 使用率显示最忙引擎；DirectX 12 能力按显卡实际查询。驱动或权限未提供的字段显示 —。")))
            .child(Button::new("github").ghost().label(tr("在 GitHub 查看源码")).on_click(|_,_,cx|cx.open_url("https://github.com/neko233-com/SystemMonitorX"))).into_any_element()
    }
}
impl Render for SystemMonitorApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .font_family(match self.resolved_language {
                Language::ZhCn => "Microsoft YaHei UI",
                Language::JaJp => "Yu Gothic UI",
                Language::KoKr => "Malgun Gothic",
                _ => "Segoe UI",
            })
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
                    .child(self.toolbar(cx))
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
                                .child(
                                    div()
                                        .id("feedback-scroll")
                                        .flex_1()
                                        .min_w_0()
                                        .max_h(px(64.))
                                        .overflow_y_scroll()
                                        .child(message.render()),
                                )
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
                                    .child(
                                        div().flex_1().child(tf(
                                            "SystemMonitorX v{} 可更新",
                                            &[(
                                                "arg0",
                                                self.update_state
                                                    .release
                                                    .as_ref()
                                                    .map(|r| r.version.as_str())
                                                    .unwrap_or("—")
                                                    .to_string(),
                                            )],
                                        )),
                                    )
                                    .child(
                                        Button::new("show-update")
                                            .small()
                                            .label(tr("查看更新"))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.set_page(Page::Settings, window, cx)
                                            })),
                                    ),
                            )
                        },
                    )
                    .child(
                        div().flex_1().min_h_0().child(match self.page {
                            Page::Processes | Page::Details => self.process_view(window, cx),
                            Page::Ports => self.ports_view(cx),
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
                                                .label(tr(if self.metadata_loading {
                                                    "读取中…"
                                                } else {
                                                    "刷新列表"
                                                }))
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
                            .child(tr(if self.paused {
                                "显示已暂停"
                            } else if self.snapshot.is_none() {
                                "正在连接系统…"
                            } else {
                                "实时采集"
                            }))
                            .child(tf(
                                "· {:.1} 秒",
                                &[(
                                    "arg0",
                                    format!("{:.1}", self.settings.interval_ms as f64 / 1000.),
                                )],
                            ))
                            .child(div().flex_1())
                            .child(
                                self.snapshot
                                    .as_ref()
                                    .map(|s| {
                                        tf(
                                            "{} · 采集 {:.1} ms · 运行时间 {}",
                                            &[
                                                ("arg0", s.host_name.to_string()),
                                                ("arg1", format!("{:.1}", s.collector_duration_ms)),
                                                ("arg2", duration(s.uptime_secs).to_string()),
                                            ],
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
            system_monitor_x::i18n::register_components(gpui_kit::component::_rust_i18n_extend);
            // The saved preference is resolved before creating input/table state.
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

fn session_protocol(value: Option<&str>) -> String {
    match value {
        Some("本地控制台") => trs("本地控制台"),
        Some(value) => {
            if let Some(code) = value
                .strip_prefix("协议 ")
                .and_then(|number| number.parse::<u16>().ok())
            {
                tf("协议 {value}", &[("value", code.to_string())])
            } else {
                value.to_owned()
            }
        }
        None => "—".to_owned(),
    }
}

#[cfg(test)]
mod settings_tests {
    use super::Settings;
    use system_monitor_x::i18n::Language;
    #[test]
    fn legacy_and_unknown_language_settings_keep_other_preferences() {
        let legacy: Settings = serde_json::from_str(r#"{"interval_ms":2000,"dark":false,"hidden_process_columns":[3,6],"group_processes":false}"#).unwrap();
        assert_eq!(legacy.language, Language::System);
        assert_eq!(legacy.interval_ms, 2000);
        assert!(!legacy.dark && !legacy.group_processes);
        for invalid in [r#""future-language""#, "null", "17", "{}"] {
            let raw = format!(
                r#"{{"language":{},"interval_ms":500,"always_on_top":true}}"#,
                invalid
            );
            let loaded: Settings = serde_json::from_str(&raw).unwrap();
            assert_eq!(loaded.language, Language::System);
            assert_eq!(loaded.interval_ms, 500);
            assert!(loaded.always_on_top);
        }
    }
    #[test]
    fn every_language_preference_round_trips_without_changing_columns() {
        for language in Language::ALL {
            let value = Settings {
                language,
                hidden_process_columns: vec![3, 8],
                ..Settings::default()
            };
            let restored: Settings =
                serde_json::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(restored.language, language);
            assert_eq!(restored.hidden_process_columns, vec![3, 8]);
        }
    }
}
