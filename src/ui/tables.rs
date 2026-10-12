use super::process_icons::{ExecutableIcon, IconCacheStats, ProcessIconCache};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{table::*, *};
use gpui_kit::{prelude::FluentBuilder as _, *};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};
use system_monitor_x::monitor::{ProcessSample, SystemSnapshot};
use system_monitor_x::name_order::{NameOrderCache, ProcessNameInput, compare_names, name_key};
use system_monitor_x::process_groups::{GroupKey, ProcessGroup, ProcessGroupKind, ProcessIdentity};
pub fn process_identity(p: &ProcessSample) -> (u32, u64) {
    (
        p.pid,
        p.start_time_windows_ticks.unwrap_or(p.start_time_unix_secs),
    )
}

pub fn bytes(value: u64) -> String {
    let value = value as f64;
    if value >= 1024.0_f64.powi(3) {
        format!("{:.1} GB", value / 1024.0_f64.powi(3))
    } else if value >= 1024.0_f64.powi(2) {
        format!("{:.1} MB", value / 1024.0_f64.powi(2))
    } else if value >= 1024.0 {
        format!("{:.1} KB", value / 1024.0)
    } else {
        format!("{value:.0} B")
    }
}
pub fn rate(value: Option<f64>) -> String {
    value
        .map(|n| format!("{}/s", bytes(n.max(0.0) as u64)))
        .unwrap_or_else(|| "—".into())
}
pub fn percent(value: Option<f32>) -> String {
    value
        .map(|n| format!("{n:.1}%"))
        .unwrap_or_else(|| "—".into())
}
pub fn duration(secs: u64) -> String {
    format!(
        "{}天 {:02}:{:02}:{:02}",
        secs / 86400,
        secs / 3600 % 24,
        secs / 60 % 60,
        secs % 60
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessRowAction {
    QueryPorts,
    CopyPid,
    CopyPath,
    CopyCommandLine,
    OpenFileLocation,
    EndProcess,
    EndProcessTree,
    EndApplication,
}
impl ProcessRowAction {
    fn menu_id(self) -> &'static str {
        match self {
            Self::QueryPorts => "process-query-ports",
            Self::CopyPid => "process-copy-pid",
            Self::CopyPath => "process-copy-path",
            Self::CopyCommandLine => "process-copy-command-line",
            Self::OpenFileLocation => "process-open-location",
            Self::EndProcess => "process-end",
            Self::EndProcessTree => "process-end-tree",
            Self::EndApplication => "process-end-application",
        }
    }
}
#[derive(Clone, Debug)]
pub struct ProcessRowTarget {
    pub pid: u32,
    pub creation_time_windows_ticks: Option<u64>,
    pub name: String,
    pub exe: Option<PathBuf>,
    pub application_targets: Option<Vec<(u32, u64)>>,
}

impl ProcessRowTarget {
    fn can_end_process(&self) -> bool {
        self.creation_time_windows_ticks
            .is_some_and(|ticks| ticks > 0)
            && !matches!(self.pid, 0 | 4)
            && self.pid != std::process::id()
    }

    fn can_end_application(&self) -> bool {
        self.can_end_process()
            && self.application_targets.as_deref().is_some_and(|targets| {
                targets.len() > 1
                    && targets.iter().all(|(pid, ticks)| {
                        *pid > 4 && *pid != std::process::id() && *ticks >= 116_444_736_000_000_000
                    })
            })
    }
}
#[derive(Clone, Debug)]
pub struct ProcessActionRequest {
    pub action: ProcessRowAction,
    pub target: ProcessRowTarget,
}
type ProcessActionHandler = Rc<dyn Fn(ProcessActionRequest, &mut Window, &mut App)>;

enum ProcessQuery<'a> {
    Empty,
    Bare(&'a str),
    Pid(u32),
    ParentPid(u32),
    NamePrefix(&'a str),
    Path(&'a str),
    Invalid,
}
impl<'a> ProcessQuery<'a> {
    fn parse(query: &'a str) -> Self {
        let query = query.trim();
        if query.is_empty() {
            return Self::Empty;
        }
        if let Some(pid) = query.strip_prefix("pid:") {
            return pid.trim().parse().map(Self::Pid).unwrap_or(Self::Invalid);
        }
        if let Some(pid) = query.strip_prefix("ppid:") {
            return pid
                .trim()
                .parse()
                .map(Self::ParentPid)
                .unwrap_or(Self::Invalid);
        }
        if let Some(prefix) = query.strip_prefix("name:") {
            return Self::NamePrefix(prefix.trim());
        }
        if let Some(path) = query.strip_prefix("path:") {
            return Self::Path(path.trim());
        }
        Self::Bare(query)
    }
}

pub struct ProcessTable {
    pub all: Vec<ProcessSample>,
    /// Visible row indices share the immutable collector snapshot. Filtering
    /// and sorting never clone every process name/path or its new detail fields.
    pub rows: Vec<usize>,
    snapshot: Option<Arc<SystemSnapshot>>,
    names: Vec<String>,
    search_keys: Vec<String>,
    path_keys: Vec<String>,
    name_order_cache: NameOrderCache,
    name_rows: Vec<usize>,
    name_sort_reused: bool,
    icons: ProcessIconCache,
    icon_sequence: u64,
    action_handler: Option<ProcessActionHandler>,
    pub query: String,
    pub sort_col: usize,
    pub descending: bool,
    pub selected: Option<(u32, u64)>,
    pub ready: bool,
    pub memory_total: u64,
    pub detailed: bool,
    pub tree_mode: bool,
    pub group_mode: bool,
    expanded_groups: HashSet<GroupKey>,
    group_headers: HashMap<usize, usize>,
    group_children: HashSet<usize>,
    collapsed: HashSet<(u32, u64)>,
    tree_rows: HashMap<usize, (usize, bool)>,
    visible_process_columns: Vec<usize>,
    visible_detail_columns: Vec<usize>,
    columns: Vec<Column>,
}
impl ProcessTable {
    pub fn new() -> Self {
        Self {
            all: vec![],
            rows: vec![],
            snapshot: None,
            names: vec![],
            search_keys: vec![],
            path_keys: vec![],
            name_order_cache: NameOrderCache::default(),
            name_rows: Vec::new(),
            name_sort_reused: false,
            icons: ProcessIconCache::new(),
            icon_sequence: 0,
            action_handler: None,
            query: String::new(),
            sort_col: 0,
            descending: false,
            selected: None,
            ready: false,
            memory_total: 0,
            detailed: false,
            tree_mode: false,
            group_mode: false,
            expanded_groups: HashSet::new(),
            group_headers: HashMap::new(),
            group_children: HashSet::new(),
            collapsed: HashSet::new(),
            tree_rows: HashMap::new(),
            visible_process_columns: (0..12).collect(),
            visible_detail_columns: (0..18).collect(),
            columns: vec![
                Column::new("name", "进程名称")
                    .width(300.)
                    .sortable()
                    .sort(ColumnSort::Ascending),
                Column::new("pid", "PID").width(74.).sortable(),
                Column::new("cpu", "CPU").width(95.).sortable(),
                Column::new("mem", "内存").width(108.).sortable(),
                Column::new("gpu", "GPU").width(90.).sortable(),
                Column::new("vram", "GPU 内存").width(110.).sortable(),
                Column::new("read", "I/O 读取").width(115.).sortable(),
                Column::new("write", "I/O 写入").width(115.).sortable(),
                Column::new("netrx", "网络接收").width(115.).sortable(),
                Column::new("nettx", "网络发送").width(115.).sortable(),
                Column::new("parent", "父进程 PID").width(100.).sortable(),
                Column::new("status", "状态").width(100.).sortable(),
                Column::new("threads", "线程").width(80.).sortable(),
                Column::new("handles", "句柄").width(95.).sortable(),
                Column::new("user", "用户").width(180.).sortable(),
                Column::new("private", "私有提交").width(120.).sortable(),
                Column::new("cputime", "CPU 时间").width(115.).sortable(),
                Column::new("session", "会话").width(85.).sortable(),
            ],
        }
    }
    /// Raw column IDs remain stable across the Processes/Details pages.
    /// This only changes presentation geometry, never process keys or ordering.
    pub fn set_hidden_columns(&mut self, hidden: &[usize]) {
        let hidden: HashSet<_> = hidden
            .iter()
            .copied()
            .filter(|index| *index > 0 && *index < self.columns.len())
            .collect();
        self.visible_process_columns = (0..12.min(self.columns.len()))
            .filter(|index| !hidden.contains(index))
            .collect();
        self.visible_detail_columns = (0..self.columns.len())
            .filter(|index| !hidden.contains(index))
            .collect();
    }

    pub fn raw_column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn column_options(&self) -> Vec<(usize, String)> {
        self.columns
            .iter()
            .take(if self.detailed {
                self.columns.len()
            } else {
                12
            })
            .enumerate()
            .map(|(raw, column)| (raw, column.name.to_string()))
            .collect()
    }

    fn visible_columns(&self) -> &[usize] {
        if self.detailed {
            &self.visible_detail_columns
        } else {
            &self.visible_process_columns
        }
    }

    /// Convert a header/cell/resize index into its stable data-column identity.
    pub fn raw_column(&self, visual: usize) -> Option<usize> {
        self.visible_columns().get(visual).copied()
    }

    /// TableEvent::ColumnWidthsChanged reports widths in visible-column order.
    /// Store them on raw columns so later hide/show and page changes keep widths.
    pub fn set_column_widths(&mut self, widths: &[Pixels]) {
        for (visual, width) in widths.iter().copied().enumerate() {
            let Some(raw) = self.raw_column(visual) else {
                break;
            };
            if !width.as_f32().is_finite() {
                continue;
            }
            let column = &mut self.columns[raw];
            column.width = width.clamp(column.min_width, column.max_width);
        }
    }

    fn visible_column(&self, visual: usize) -> Option<Column> {
        let raw = self.raw_column(visual)?;
        let mut column = self.columns[raw].clone();
        if raw == 4 && self.groups_active() {
            column.name = "GPU（成员最大）".into();
        }
        column.sort = Some(if raw == self.sort_col {
            if self.descending {
                ColumnSort::Descending
            } else {
                ColumnSort::Ascending
            }
        } else {
            ColumnSort::Default
        });
        Some(column)
    }

    fn sort_visible_column(&mut self, visual: usize, sort: ColumnSort) -> bool {
        let Some(raw) = self.raw_column(visual) else {
            return false;
        };
        self.sort_col = raw;
        self.descending = matches!(sort, ColumnSort::Descending);
        self.rebuild();
        true
    }

    pub fn set_snapshot(&mut self, snapshot: Arc<SystemSnapshot>) {
        self.ready = snapshot.cpu_ready;
        self.memory_total = snapshot.memory_total;
        self.snapshot = Some(snapshot);
        self.prepare_keys();
        self.rebuild();
    }
    fn samples(&self) -> &[ProcessSample] {
        self.snapshot
            .as_ref()
            .map(|s| s.processes.as_slice())
            .unwrap_or(&self.all)
    }
    pub fn row(&self, row: usize) -> Option<&ProcessSample> {
        self.rows
            .get(row)
            .and_then(|index| self.samples().get(*index))
    }
    fn icon_element(&self, process: &ProcessSample) -> AnyElement {
        let icon = process
            .exe
            .as_ref()
            .and_then(|path| self.icons.request(path));
        div()
            .size(px(18.))
            .flex_shrink_0()
            .when_some(icon, |element, icon| {
                element.child(img(icon.image.clone()).size(px(18.)))
            })
            .into_any_element()
    }
    fn name_element(&self, process: &ProcessSample) -> AnyElement {
        h_flex()
            .gap_2()
            .items_center()
            .text_sm()
            .child(self.icon_element(process))
            .child(process.name.clone())
            .into_any_element()
    }
    fn prepare_keys(&mut self) {
        self.names = self.samples().iter().map(|p| name_key(&p.name)).collect();
        self.search_keys = self
            .samples()
            .iter()
            .map(|p| {
                format!(
                    "{}\n{}\n{}\n{}",
                    p.name,
                    p.pid,
                    p.exe
                        .as_ref()
                        .map(|p| p.to_string_lossy())
                        .unwrap_or_default(),
                    p.user_name.as_deref().unwrap_or("")
                )
                .to_lowercase()
            })
            .collect();
        self.path_keys = self
            .samples()
            .iter()
            .map(|process| {
                process
                    .exe
                    .as_ref()
                    .map(|path| name_key(&path.to_string_lossy()))
                    .unwrap_or_default()
            })
            .collect();
        let snapshot = self.snapshot.clone();
        let samples = snapshot
            .as_ref()
            .map(|snapshot| snapshot.processes.as_slice())
            .unwrap_or(&self.all);
        let inputs: Vec<_> = samples
            .iter()
            .map(|process| ProcessNameInput {
                name: &process.name,
                pid: process.pid,
                creation_time_windows_ticks: process.start_time_windows_ticks,
                start_time_unix_secs: process.start_time_unix_secs,
            })
            .collect();
        let ordered = self.name_order_cache.order(&inputs);
        self.name_rows = ordered.indices;
        self.name_sort_reused = ordered.reused;
    }
    fn matches_query(&self, index: usize, query: &ProcessQuery<'_>) -> bool {
        let sample = &self.samples()[index];
        match query {
            ProcessQuery::Empty => true,
            ProcessQuery::Bare(query) => self.search_keys[index].contains(query),
            ProcessQuery::Pid(pid) => sample.pid == *pid,
            ProcessQuery::ParentPid(pid) => sample.parent_pid == Some(*pid),
            ProcessQuery::NamePrefix(prefix) => self.names[index].starts_with(prefix),
            ProcessQuery::Path(path) => self.path_keys[index].contains(path),
            ProcessQuery::Invalid => false,
        }
    }
    #[cfg(any(test, feature = "ui-test"))]
    pub fn name_order_reused(&self) -> bool {
        self.name_sort_reused
    }
    #[cfg(any(test, feature = "ui-test"))]
    pub fn name_sort_count(&self) -> u64 {
        self.name_order_cache.sort_count()
    }
    pub fn icon_cache_stats(&self) -> Option<IconCacheStats> {
        self.icons.stats()
    }
    pub fn request_process_icon(&self, path: &Path) -> Option<Arc<ExecutableIcon>> {
        self.icons.request(path)
    }
    pub fn poll_icon_updates(&mut self) -> bool {
        let sequence = self.icons.sequence();
        let changed = sequence != self.icon_sequence;
        self.icon_sequence = sequence;
        changed
    }
    pub fn set_action_handler(
        &mut self,
        handler: impl Fn(ProcessActionRequest, &mut Window, &mut App) + 'static,
    ) {
        self.action_handler = Some(Rc::new(handler));
    }
    /// Menu captures this clicked target, independently of later selection.
    pub fn action_target(&self, row: usize) -> Option<ProcessRowTarget> {
        let index = *self.rows.get(row)?;
        let process = self.samples().get(index)?;
        let application_targets = self.group_header(index).and_then(|group| {
            if group.kind != ProcessGroupKind::Application || group.indices.len() <= 1 {
                return None;
            }
            let root = ProcessIdentity::from_sample(process)?;
            if group.root_identity != Some(root) || group.key.exact_identity() != Some(root) {
                return None;
            }
            group
                .indices
                .iter()
                .map(|index| {
                    let member = ProcessIdentity::from_sample(self.samples().get(*index)?)?;
                    Some((member.pid, member.creation_time_windows_ticks))
                })
                .collect()
        });
        Some(ProcessRowTarget {
            pid: process.pid,
            creation_time_windows_ticks: process
                .start_time_windows_ticks
                .filter(|ticks| *ticks > 0),
            name: process.name.clone(),
            exe: process.exe.clone(),
            application_targets,
        })
    }

    pub fn rebuild(&mut self) {
        // Standalone model tests may replace their owned vector directly.
        if self.snapshot.is_none() {
            self.prepare_keys();
        }
        let query = name_key(&self.query);
        let parsed = ProcessQuery::parse(&query);
        let mut rows: Vec<usize> = if self.sort_col == 0 {
            self.name_rows
                .iter()
                .copied()
                .filter(|index| self.matches_query(*index, &parsed))
                .collect()
        } else {
            (0..self.samples().len())
                .filter(|index| self.matches_query(*index, &parsed))
                .collect()
        };
        if self.sort_col == 0 && self.descending {
            let mut blocks = Vec::new();
            let mut start = 0;
            while start < rows.len() {
                let mut end = start + 1;
                while end < rows.len() && self.names[rows[end]] == self.names[rows[start]] {
                    end += 1;
                }
                blocks.push(start..end);
                start = end;
            }
            rows = blocks
                .into_iter()
                .rev()
                .flat_map(|range| rows[range].iter().copied())
                .collect();
        }
        let (col, descending) = (self.sort_col, self.descending);
        let samples = self.samples();
        if col != 0 {
            rows.sort_unstable_by(|ia, ib| {
                let (a, b) = (&samples[*ia], &samples[*ib]);
                let order = match col {
                    0 => self.names[*ia].cmp(&self.names[*ib]),
                    1 => a.pid.cmp(&b.pid),
                    2 => a.cpu_usage.total_cmp(&b.cpu_usage),
                    3 => a.memory_bytes.cmp(&b.memory_bytes),
                    4 => compare_optional(a.gpu_usage, b.gpu_usage),
                    5 => a.gpu_memory_bytes.cmp(&b.gpu_memory_bytes),
                    6 => compare_optional(a.disk_read_bytes_per_sec, b.disk_read_bytes_per_sec),
                    7 => compare_optional(a.disk_write_bytes_per_sec, b.disk_write_bytes_per_sec),
                    8 => compare_optional(
                        a.network_received_bytes_per_sec,
                        b.network_received_bytes_per_sec,
                    ),
                    9 => compare_optional(
                        a.network_transmitted_bytes_per_sec,
                        b.network_transmitted_bytes_per_sec,
                    ),
                    10 => a.parent_pid.cmp(&b.parent_pid),
                    12 => a.thread_count.cmp(&b.thread_count),
                    13 => a.handle_count.cmp(&b.handle_count),
                    14 => a.user_name.cmp(&b.user_name),
                    15 => a.private_bytes.cmp(&b.private_bytes),
                    16 => compare_optional(a.cpu_time_seconds, b.cpu_time_seconds),
                    17 => a.session_id.cmp(&b.session_id),
                    _ => a.status.cmp(&b.status),
                };
                let order = if descending { order.reverse() } else { order };
                order
                    .then(a.pid.cmp(&b.pid))
                    .then(process_identity(a).1.cmp(&process_identity(b).1))
            });
        }
        self.tree_rows.clear();
        self.group_headers.clear();
        self.group_children.clear();
        if self.groups_active() {
            let (grouped, headers, children) = self.grouped_rows(&rows);
            rows = grouped;
            self.group_headers = headers;
            self.group_children = children;
            let current: HashSet<_> = self
                .application_groups()
                .iter()
                .filter(|group| group.kind == ProcessGroupKind::Application)
                .map(|group| group.key.clone())
                .collect();
            self.expanded_groups.retain(|key| current.contains(key));
        } else if self.tree_mode && query.is_empty() {
            let tree =
                system_monitor_x::process_tree::flatten(self.samples(), &rows, &self.collapsed);
            rows = tree.iter().map(|row| row.index).collect();
            self.tree_rows = tree
                .into_iter()
                .map(|row| (row.index, (row.depth, row.has_children)))
                .collect();
        }
        self.rows = rows;
        if self.selected.is_some_and(|id| {
            !self
                .rows
                .iter()
                .any(|index| process_identity(&self.samples()[*index]) == id)
        }) {
            self.selected = None;
        }
    }
    fn application_groups(&self) -> &[ProcessGroup] {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.application_groups.as_slice())
            .unwrap_or(&[])
    }

    fn groups_active(&self) -> bool {
        self.group_mode && !self.detailed && !self.tree_mode && self.query.is_empty()
    }

    fn group_header(&self, index: usize) -> Option<&ProcessGroup> {
        self.group_headers
            .get(&index)
            .and_then(|group| self.application_groups().get(*group))
    }

    /// Default is folded; only exact group keys persist across snapshots.
    pub fn toggle_application_group(&mut self, key: &GroupKey) {
        if key.exact_identity().is_none() {
            return;
        }
        if !self.expanded_groups.remove(key) {
            self.expanded_groups.insert(key.clone());
        }
        self.rebuild();
    }

    /// Only an explicitly selected multi-process application header authorizes
    /// group operations. A selected child/search result still means one process.
    /// Targets always use exact FILETIME identities, never rounded Unix seconds.
    pub fn selected_application_targets(&self) -> Option<Vec<(u32, u64)>> {
        if !self.groups_active() {
            return None;
        }
        let index = *self.rows.get(self.selected_index()?)?;
        let group = self.group_header(index)?;
        if group.kind != ProcessGroupKind::Application || group.indices.len() <= 1 {
            return None;
        }
        let root = ProcessIdentity::from_sample(self.samples().get(group.root_index)?)?;
        if group.root_identity != Some(root) || group.key.exact_identity() != Some(root) {
            return None;
        }
        group
            .indices
            .iter()
            .map(|index| {
                let member = ProcessIdentity::from_sample(self.samples().get(*index)?)?;
                Some((member.pid, member.creation_time_windows_ticks))
            })
            .collect()
    }

    pub fn selected_application_unavailable_reason(&self) -> Option<String> {
        if !self.groups_active() {
            return None;
        }
        let index = *self.rows.get(self.selected_index()?)?;
        let group = self.group_header(index)?;
        (group.kind == ProcessGroupKind::Application
            && group.indices.len() > 1
            && self.selected_application_targets().is_none())
        .then(|| "应用成员的精确进程身份不可用；请展开后逐个选择可核验进程。".into())
    }

    fn compare_grouped_rows(
        &self,
        ia: usize,
        ib: usize,
        headers: &HashMap<usize, usize>,
    ) -> std::cmp::Ordering {
        let (a, b) = (&self.samples()[ia], &self.samples()[ib]);
        let ga = headers
            .get(&ia)
            .and_then(|index| self.application_groups().get(*index));
        let gb = headers
            .get(&ib)
            .and_then(|index| self.application_groups().get(*index));
        let order = match self.sort_col {
            0 => self.names[ia].cmp(&self.names[ib]),
            1 => a.pid.cmp(&b.pid),
            2 => compare_optional(
                ga.map_or(Some(a.cpu_usage), |group| group.aggregates.cpu_usage),
                gb.map_or(Some(b.cpu_usage), |group| group.aggregates.cpu_usage),
            ),
            3 => ga
                .map_or(Some(a.memory_bytes), |group| group.aggregates.memory_bytes)
                .cmp(&gb.map_or(Some(b.memory_bytes), |group| group.aggregates.memory_bytes)),
            4 => compare_optional(
                ga.map_or(a.gpu_usage, |group| group.aggregates.gpu_usage_max),
                gb.map_or(b.gpu_usage, |group| group.aggregates.gpu_usage_max),
            ),
            5 => ga
                .map_or(a.gpu_memory_bytes, |group| {
                    group.aggregates.gpu_memory_bytes
                })
                .cmp(&gb.map_or(b.gpu_memory_bytes, |group| {
                    group.aggregates.gpu_memory_bytes
                })),
            6 => compare_optional(
                ga.map_or(a.disk_read_bytes_per_sec, |group| {
                    group.aggregates.disk_read_bytes_per_sec
                }),
                gb.map_or(b.disk_read_bytes_per_sec, |group| {
                    group.aggregates.disk_read_bytes_per_sec
                }),
            ),
            7 => compare_optional(
                ga.map_or(a.disk_write_bytes_per_sec, |group| {
                    group.aggregates.disk_write_bytes_per_sec
                }),
                gb.map_or(b.disk_write_bytes_per_sec, |group| {
                    group.aggregates.disk_write_bytes_per_sec
                }),
            ),
            8 => compare_optional(
                ga.map_or(a.network_received_bytes_per_sec, |group| {
                    group.aggregates.network_received_bytes_per_sec
                }),
                gb.map_or(b.network_received_bytes_per_sec, |group| {
                    group.aggregates.network_received_bytes_per_sec
                }),
            ),
            9 => compare_optional(
                ga.map_or(a.network_transmitted_bytes_per_sec, |group| {
                    group.aggregates.network_transmitted_bytes_per_sec
                }),
                gb.map_or(b.network_transmitted_bytes_per_sec, |group| {
                    group.aggregates.network_transmitted_bytes_per_sec
                }),
            ),
            10 => a.parent_pid.cmp(&b.parent_pid),
            _ => a.status.cmp(&b.status),
        };
        let order = if self.descending {
            order.reverse()
        } else {
            order
        };
        order
            .then(a.pid.cmp(&b.pid))
            .then(process_identity(a).1.cmp(&process_identity(b).1))
    }

    fn grouped_rows(
        &self,
        raw_rows: &[usize],
    ) -> (Vec<usize>, HashMap<usize, usize>, HashSet<usize>) {
        let mut membership = vec![None; self.samples().len()];
        let mut headers = HashMap::new();
        let groups = self.application_groups();
        for (group_index, group) in groups.iter().enumerate() {
            if group.kind != ProcessGroupKind::Application
                || group.indices.len() <= 1
                || !group.indices.contains(&group.root_index)
                || self
                    .samples()
                    .get(group.root_index)
                    .and_then(ProcessIdentity::from_sample)
                    != group.root_identity
                || group.key.exact_identity() != group.root_identity
                || group.root_identity.is_none()
            {
                continue;
            }
            let mut distinct = HashSet::new();
            if !group.indices.iter().all(|index| {
                *index < membership.len() && membership[*index].is_none() && distinct.insert(*index)
            }) {
                continue; // Malformed metadata falls back to every raw row.
            }
            for &index in &group.indices {
                membership[index] = Some(group_index);
            }
            headers.insert(group.root_index, group_index);
        }
        let mut member_rows = vec![Vec::new(); groups.len()];
        let mut top_rows = Vec::with_capacity(raw_rows.len());
        for &index in raw_rows {
            if let Some(group_index) = membership[index]
                && index != groups[group_index].root_index
            {
                member_rows[group_index].push(index);
                continue;
            }
            top_rows.push(index);
        }
        // Name-ordered raw roots already have their stable order.
        if self.sort_col != 0 {
            top_rows.sort_unstable_by(|a, b| self.compare_grouped_rows(*a, *b, &headers));
        }
        let mut rows = Vec::with_capacity(raw_rows.len());
        let mut children = HashSet::new();
        for root in top_rows {
            rows.push(root);
            if let Some(group_index) = headers.get(&root)
                && self.expanded_groups.contains(&groups[*group_index].key)
            {
                for &child in &member_rows[*group_index] {
                    rows.push(child);
                    children.insert(child);
                }
            }
        }
        (rows, headers, children)
    }

    pub fn selection(&self) -> Option<&ProcessSample> {
        self.selected.and_then(|id| {
            self.rows.iter().find_map(|index| {
                let p = &self.samples()[*index];
                (process_identity(p) == id).then_some(p)
            })
        })
    }
    pub fn selected_index(&self) -> Option<usize> {
        self.selected.and_then(|id| {
            self.rows
                .iter()
                .position(|index| process_identity(&self.samples()[*index]) == id)
        })
    }
}

pub fn sync_selection(
    table: &mut TableState<ProcessTable>,
    cx: &mut Context<TableState<ProcessTable>>,
) {
    let index = table.delegate().selected_index();
    if index != table.selected_row() {
        if let Some(index) = index {
            table.set_selected_row(index, cx);
        } else {
            table.clear_selection(cx);
        }
    }
}
fn compare_optional<T: PartialOrd>(a: Option<T>, b: Option<T>) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}
impl TableDelegate for ProcessTable {
    fn columns_count(&self, _: &App) -> usize {
        self.visible_columns().len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, i: usize, _: &App) -> Column {
        self.visible_column(i)
            .unwrap_or_else(|| self.columns[0].clone())
    }
    fn context_menu(
        &mut self,
        row: usize,
        mut menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(handler) = self.action_handler.clone() else {
            return menu;
        };
        let Some(target) = self.action_target(row) else {
            return menu;
        };
        let verified = target.creation_time_windows_ticks.is_some();
        let can_end = target.can_end_process();
        let has_path = target.exe.is_some();
        menu = menu.label(format!("{} · PID {}", target.name, target.pid));
        let target = Rc::new(target);
        let items = [
            (ProcessRowAction::QueryPorts, "查询该 PID 的端口", true),
            (ProcessRowAction::CopyPid, "复制 PID", true),
            (ProcessRowAction::CopyPath, "复制路径", has_path),
            (ProcessRowAction::CopyCommandLine, "复制命令行", verified),
            (ProcessRowAction::OpenFileLocation, "打开文件位置", has_path),
            (ProcessRowAction::EndProcess, "结束进程", can_end),
            (ProcessRowAction::EndProcessTree, "结束进程树", can_end),
            (
                ProcessRowAction::EndApplication,
                "结束应用",
                target.can_end_application(),
            ),
        ];
        for (action, label, enabled) in items {
            if action == ProcessRowAction::EndApplication && target.application_targets.is_none() {
                continue;
            }
            if action == ProcessRowAction::EndProcess {
                menu = menu.separator();
            }
            let target = target.clone();
            let handler = handler.clone();
            menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    div()
                        .id(action.menu_id())
                        .w_full()
                        .text_sm()
                        .child(label)
                        .test_support()
                })
                .disabled(!enabled)
                .on_click(move |_, window, cx| {
                    handler(
                        ProcessActionRequest {
                            action,
                            target: (*target).clone(),
                        },
                        window,
                        cx,
                    );
                }),
            );
        }
        menu
    }
    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(col) = self.raw_column(col) else {
            return div().into_any_element();
        };
        let index = self.rows[row];
        let (depth, has_children) = self.tree_rows.get(&index).copied().unwrap_or_default();
        let p = self.row(row).expect("table row within snapshot");
        let identity = process_identity(p);
        if col == 0
            && let Some(group) = self.group_header(index)
        {
            let key = group.key.clone();
            let expanded = self.expanded_groups.contains(&key);
            let name = format!("{} ({})", group.display_name, group.indices.len());
            return h_flex()
                .gap_1()
                .pl_2()
                .child(
                    div()
                        .id(("app-collapse", identity.0 as usize))
                        .w(px(18.))
                        .cursor_pointer()
                        .child(if expanded { "▾" } else { "▸" })
                        .on_click(cx.listener(move |table, _, _, cx| {
                            table.delegate_mut().toggle_application_group(&key);
                            sync_selection(table, cx);
                            cx.stop_propagation();
                            cx.notify();
                        })),
                )
                .child(self.icon_element(p))
                .child(name)
                .into_any_element();
        }
        if col == 0 && self.group_children.contains(&index) {
            return h_flex()
                .pl(px(34.))
                .child(self.name_element(p))
                .into_any_element();
        }
        if col == 0 && self.tree_mode && self.query.is_empty() {
            let collapsed = self.collapsed.contains(&identity);
            return h_flex()
                .gap_1()
                .pl(px((depth.min(20) as f32) * 14. + 8.))
                .when(has_children, |row| {
                    row.child(
                        div()
                            .id(("collapse", identity.0 as usize))
                            .w(px(18.))
                            .cursor_pointer()
                            .child(if collapsed { "▸" } else { "▾" })
                            .on_click(cx.listener(move |table, _, _, cx| {
                                let delegate = table.delegate_mut();
                                if !delegate.collapsed.remove(&identity) {
                                    delegate.collapsed.insert(identity);
                                }
                                delegate.rebuild();
                                sync_selection(table, cx);
                                cx.stop_propagation();
                                cx.notify();
                            })),
                    )
                })
                .when(!has_children, |row| row.child(div().w(px(18.))))
                .child(self.name_element(p))
                .into_any_element();
        }
        if col == 0 {
            return div()
                .size_full()
                .flex()
                .items_center()
                .px_2()
                .child(self.name_element(p))
                .into_any_element();
        }
        let text = if let Some(group) = self.group_header(index) {
            let aggregate = &group.aggregates;
            match col {
                1 => p.pid.to_string(),
                2 => {
                    if self.ready {
                        percent(aggregate.cpu_usage)
                    } else {
                        "采样中".into()
                    }
                }
                3 => aggregate
                    .memory_bytes
                    .map(bytes)
                    .unwrap_or_else(|| "—".into()),
                4 => percent(aggregate.gpu_usage_max),
                5 => aggregate
                    .gpu_memory_bytes
                    .map(bytes)
                    .unwrap_or_else(|| "—".into()),
                6 => rate(aggregate.disk_read_bytes_per_sec),
                7 => rate(aggregate.disk_write_bytes_per_sec),
                8 => rate(aggregate.network_received_bytes_per_sec),
                9 => rate(aggregate.network_transmitted_bytes_per_sec),
                10 => p
                    .parent_pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "—".into()),
                _ => format!("应用 · {} 个进程", group.indices.len()),
            }
        } else {
            match col {
                0 => p.name.clone(),
                1 => p.pid.to_string(),
                2 => {
                    if self.ready {
                        format!("{:.1}%", p.cpu_usage)
                    } else {
                        "采样中".into()
                    }
                }
                3 => bytes(p.memory_bytes),
                4 => percent(p.gpu_usage),
                5 => p.gpu_memory_bytes.map(bytes).unwrap_or_else(|| "—".into()),
                6 => rate(p.disk_read_bytes_per_sec),
                7 => rate(p.disk_write_bytes_per_sec),
                8 => rate(p.network_received_bytes_per_sec),
                9 => rate(p.network_transmitted_bytes_per_sec),
                10 => p
                    .parent_pid
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "—".into()),
                12 => p
                    .thread_count
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
                13 => p
                    .handle_count
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
                14 => p.user_name.clone().unwrap_or_else(|| "—".into()),
                15 => p.private_bytes.map(bytes).unwrap_or_else(|| "—".into()),
                16 => p
                    .cpu_time_seconds
                    .map(|n| format!("{n:.2} s"))
                    .unwrap_or_else(|| "—".into()),
                17 => p
                    .session_id
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
                _ => p.status.clone(),
            }
        };
        let intensity = if let Some(group) = self.group_header(index) {
            match col {
                2 => group.aggregates.cpu_usage.unwrap_or(0.) / 100.,
                3 => (group.aggregates.memory_bytes.unwrap_or(0) as f32
                    / self.memory_total.max(1) as f32)
                    .min(1.),
                4 => group.aggregates.gpu_usage_max.unwrap_or(0.) / 100.,
                _ => 0.,
            }
        } else {
            match col {
                2 => p.cpu_usage / 100.,
                3 => (p.memory_bytes as f32 / self.memory_total.max(1) as f32).min(1.),
                4 => p.gpu_usage.unwrap_or(0.) / 100.,
                _ => 0.,
            }
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .px_2()
            .text_sm()
            .truncate()
            .text_color(if col == 1 || col >= 8 {
                cx.theme().muted_foreground
            } else {
                cx.theme().foreground
            })
            .when(col > 0 && col < 8, |el| {
                el.justify_end().font_family("Consolas")
            })
            .when(intensity > 0.005, |el| {
                el.bg(gpui_kit::rgba(0x337bceff).opacity(0.08 + intensity * 0.4))
            })
            .child(text)
            .into_any_element()
    }
    fn perform_sort(
        &mut self,
        col: usize,
        sort: ColumnSort,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        if !self.sort_visible_column(col, sort) {
            return;
        }
        cx.defer_in(window, |table, _, cx| sync_selection(table, cx));
        cx.notify();
    }
    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_8()
            .text_color(cx.theme().muted_foreground)
            .child("没有匹配的进程")
    }
}

pub struct TextTable {
    pub all: Vec<Vec<String>>,
    pub rows: Vec<Vec<String>>,
    pub query: String,
    columns: Vec<Column>,
    sort: Option<(usize, bool)>,
    numeric: Vec<usize>,
    identity_columns: Vec<usize>,
}
impl TextTable {
    pub fn new(columns: &[(&'static str, f32)]) -> Self {
        Self {
            all: vec![],
            rows: vec![],
            query: String::new(),
            sort: Some((0, false)),
            numeric: vec![],
            identity_columns: Vec::new(),
            columns: columns
                .iter()
                .enumerate()
                .map(|(i, (s, w))| Column::new(format!("col{i}"), *s).width(*w).sortable())
                .collect(),
        }
    }
    /// Tie-break equal names with stable identity columns, not changing metrics.
    pub fn stable_identity_columns(mut self, columns: &[usize]) -> Self {
        self.identity_columns = columns
            .iter()
            .copied()
            .filter(|column| *column < self.columns.len())
            .collect();
        self
    }
    fn stable_secondary(
        a: &[String],
        b: &[String],
        identity_columns: &[usize],
        numeric_columns: &[usize],
    ) -> std::cmp::Ordering {
        if identity_columns.is_empty() {
            return a
                .iter()
                .skip(1)
                .zip(b.iter().skip(1))
                .map(|(left, right)| compare_names(left, right))
                .find(|order| !order.is_eq())
                .unwrap_or_else(|| a.len().cmp(&b.len()));
        }
        for &column in identity_columns {
            let Some((left, right)) = a.get(column).zip(b.get(column)) else {
                continue;
            };
            let order = if numeric_columns.contains(&column) {
                compare_optional(numeric_value(left), numeric_value(right))
            } else {
                compare_names(left, right)
            };
            if !order.is_eq() {
                return order;
            }
        }
        std::cmp::Ordering::Equal
    }
    pub fn numeric_columns(mut self, columns: &[usize]) -> Self {
        self.numeric = columns.to_vec();
        self
    }
    pub fn rebuild(&mut self) {
        let q = name_key(&self.query);
        self.rows = self
            .all
            .iter()
            .filter(|r| r.iter().any(|s| name_key(s).contains(&q)))
            .cloned()
            .collect();
        if let Some((column, descending)) = self.sort {
            let numeric = self.numeric.contains(&column);
            let identity_columns = &self.identity_columns;
            let numeric_columns = &self.numeric;
            self.rows.sort_by(|a, b| {
                let order = if numeric {
                    compare_optional(numeric_value(&a[column]), numeric_value(&b[column]))
                } else {
                    compare_names(&a[column], &b[column])
                };
                let order = if descending { order.reverse() } else { order };
                order.then_with(|| Self::stable_secondary(a, b, identity_columns, numeric_columns))
            });
        }
    }
}
fn numeric_value(value: &str) -> Option<f64> {
    let mut parts = value.split_whitespace();
    let number = parts.next()?.trim_end_matches('%').parse::<f64>().ok()?;
    let scale = match parts.next().unwrap_or("") {
        "KB" | "KB/s" => 1024.,
        "MB" | "MB/s" => 1024. * 1024.,
        "GB" | "GB/s" => 1024. * 1024. * 1024.,
        _ => 1.,
    };
    let result = number * scale;
    result.is_finite().then_some(result)
}
impl TableDelegate for TextTable {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, i: usize, _: &App) -> Column {
        self.columns[i].clone()
    }
    fn render_td(
        &mut self,
        r: usize,
        c: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .px_2()
            .text_sm()
            .truncate()
            .text_color(if c == 0 {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            })
            .child(self.rows[r][c].clone())
    }
    fn perform_sort(
        &mut self,
        c: usize,
        s: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.sort = Some((c, matches!(s, ColumnSort::Descending)));
        self.rebuild();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{ProcessTable, bytes};
    use gpui_kit::{component::table::ColumnSort, px};
    use system_monitor_x::monitor::ProcessSample;
    #[test]
    fn selection_does_not_follow_reused_pid() {
        let mut t = ProcessTable::new();
        t.all = vec![ProcessSample {
            pid: 123,
            start_time_unix_secs: 200,
            name: "replacement.exe".into(),
            ..Default::default()
        }];
        t.selected = Some((123, 100));
        t.rebuild();
        assert!(t.selection().is_none());
    }
    #[test]
    fn filters_by_pid_and_sorts_without_truncation() {
        let mut t = ProcessTable::new();
        t.sort_col = 2;
        t.descending = true;
        t.all = (1..=400)
            .map(|pid| ProcessSample {
                pid,
                cpu_usage: pid as f32,
                name: format!("app-{pid}.exe"),
                ..Default::default()
            })
            .collect();
        t.rebuild();
        assert_eq!(t.rows.len(), 400);
        assert_eq!(t.row(0).unwrap().pid, 400);
        t.query = "app-123".into();
        t.rebuild();
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.row(0).unwrap().pid, 123);
        assert_eq!(bytes(1024 * 1024), "1.0 MB");
    }
    #[test]
    fn filter_clears_hidden_selection_and_refresh_remaps_identity() {
        let mut t = ProcessTable::new();
        t.sort_col = 2;
        t.descending = true;
        t.all = vec![
            ProcessSample {
                pid: 1,
                start_time_unix_secs: 10,
                cpu_usage: 1.,
                name: "one.exe".into(),
                ..Default::default()
            },
            ProcessSample {
                pid: 2,
                start_time_unix_secs: 20,
                cpu_usage: 2.,
                name: "two.exe".into(),
                ..Default::default()
            },
        ];
        t.selected = Some((1, 10));
        t.rebuild();
        assert_eq!(t.selected_index(), Some(1));
        t.all[0].cpu_usage = 3.;
        t.rebuild();
        assert_eq!(t.selected_index(), Some(0));
        t.query = "two.exe".into();
        t.rebuild();
        assert!(t.selection().is_none());
    }
    #[test]
    fn text_sort_persists_after_filter_or_refresh() {
        let mut t = super::TextTable::new(&[("Name", 100.)]);
        t.all = vec![vec!["A".into()], vec!["C".into()], vec!["B".into()]];
        t.sort = Some((0, true));
        t.rebuild();
        assert_eq!(t.rows[0][0], "C");
        t.query = "B".into();
        t.rebuild();
        assert_eq!(t.rows[0][0], "B");
        t.query.clear();
        t.all.push(vec!["D".into()]);
        t.rebuild();
        assert_eq!(t.rows[0][0], "D");
    }
    #[test]
    fn exact_identity_rejects_reused_pid_within_one_unix_second() {
        let mut table = ProcessTable::new();
        table.all = vec![ProcessSample {
            pid: 87,
            start_time_unix_secs: 100,
            start_time_windows_ticks: Some(116_444_737_000_000_002),
            ..Default::default()
        }];
        table.selected = Some((87, 116_444_737_000_000_001));
        table.rebuild();
        assert!(table.selection().is_none());
        assert!(table.selected.is_none());
    }
    #[test]
    fn text_numeric_sort_compares_units_and_rejects_unavailable_values() {
        use super::{TextTable, numeric_value};
        assert_eq!(numeric_value("1.5 GB"), Some(1.5 * 1024.0_f64.powi(3)));
        assert_eq!(numeric_value("—"), None);
        assert_eq!(numeric_value("NaN"), None);
        let mut table = TextTable::new(&[("usage", 100.)]).numeric_columns(&[0]);
        table.all = vec![
            vec!["900 MB".into()],
            vec!["2 GB".into()],
            vec!["100 KB".into()],
        ];
        table.sort = Some((0, true));
        table.rebuild();
        assert_eq!(table.rows[0][0], "2 GB");
        assert_eq!(table.rows[2][0], "100 KB");
    }
    #[test]
    fn filters_users_and_sorts_measured_process_network() {
        let mut table = ProcessTable::new();
        table.all = vec![
            ProcessSample {
                pid: 8,
                user_name: Some("DOMAIN\\alice".into()),
                network_received_bytes_per_sec: Some(1_000.0),
                ..Default::default()
            },
            ProcessSample {
                pid: 9,
                user_name: Some("DOMAIN\\alice".into()),
                network_received_bytes_per_sec: Some(20_000.0),
                ..Default::default()
            },
            ProcessSample {
                pid: 10,
                user_name: Some("DOMAIN\\bob".into()),
                ..Default::default()
            },
        ];
        table.query = "ALICE".into();
        table.sort_col = 8;
        table.descending = true;
        table.rebuild();
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.row(0).unwrap().pid, 9);
        assert_eq!(table.row(1).unwrap().pid, 8);
    }

    #[test]
    fn hiding_columns_keeps_name_and_filters_each_page_without_touching_selection() {
        let mut table = ProcessTable::new();
        table.all = vec![ProcessSample {
            pid: 12,
            name: "selected.exe".into(),
            start_time_windows_ticks: Some(116_444_736_000_000_123),
            ..Default::default()
        }];
        table.selected = Some((12, 116_444_736_000_000_123));
        table.rebuild();
        let rows = table.rows.clone();
        let names = table.names.as_ptr();
        let search_keys = table.search_keys.as_ptr();
        table.set_hidden_columns(&[0, 1, 1, 3, 12, 17, 18, usize::MAX]);
        assert_eq!(table.visible_columns(), &[0, 2, 4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(table.column_options().len(), 12);
        table.detailed = true;
        assert_eq!(
            table.visible_columns(),
            &[0, 2, 4, 5, 6, 7, 8, 9, 10, 11, 13, 14, 15, 16]
        );
        assert_eq!(table.column_options().len(), 18);
        assert_eq!(table.rows, rows);
        assert_eq!(table.names.as_ptr(), names);
        assert_eq!(table.search_keys.as_ptr(), search_keys);
        assert_eq!(table.selection().unwrap().pid, 12);
        assert_eq!(table.selected_index(), Some(0));
        table.set_hidden_columns(&(0..18).collect::<Vec<_>>());
        assert_eq!(table.visible_columns(), &[0]);
        assert_eq!(table.sort_col, 0);
    }

    #[test]
    fn visible_cpu_and_network_headers_sort_their_raw_numeric_fields() {
        let mut table = ProcessTable::new();
        table.all = vec![
            ProcessSample {
                pid: 1,
                cpu_usage: 1.0,
                network_received_bytes_per_sec: Some(50_000.0),
                ..Default::default()
            },
            ProcessSample {
                pid: 2,
                cpu_usage: 90.0,
                network_received_bytes_per_sec: Some(1_000.0),
                ..Default::default()
            },
        ];
        table.set_hidden_columns(&[1, 3, 4, 5]);
        assert_eq!(table.raw_column(1), Some(2));
        assert_eq!(table.visible_column(1).unwrap().key.as_ref(), "cpu");
        assert!(table.sort_visible_column(1, ColumnSort::Descending));
        assert_eq!(table.sort_col, 2);
        assert_eq!(table.row(0).unwrap().pid, 2);
        assert_eq!(table.raw_column(4), Some(8));
        assert_eq!(table.visible_column(4).unwrap().key.as_ref(), "netrx");
        assert!(table.sort_visible_column(4, ColumnSort::Descending));
        assert_eq!(table.sort_col, 8);
        assert_eq!(table.row(0).unwrap().pid, 1);
        assert!(matches!(
            table.visible_column(4).unwrap().sort,
            Some(ColumnSort::Descending)
        ));
        assert!(matches!(
            table.visible_column(1).unwrap().sort,
            Some(ColumnSort::Default)
        ));
        let old_rows = table.rows.clone();
        assert!(!table.sort_visible_column(usize::MAX, ColumnSort::Ascending));
        assert_eq!(table.rows, old_rows);
    }

    #[test]
    fn detailed_numeric_columns_and_resizing_keep_raw_identity() {
        let mut table = ProcessTable::new();
        table.detailed = true;
        table.all = vec![
            ProcessSample {
                pid: 1,
                private_bytes: Some(50_000),
                cpu_time_seconds: Some(1.0),
                ..Default::default()
            },
            ProcessSample {
                pid: 2,
                private_bytes: Some(1_000),
                cpu_time_seconds: Some(70.0),
                ..Default::default()
            },
        ];
        table.set_hidden_columns(&[1, 3, 4, 5, 12, 13, 14]);
        let private_visual = table
            .visible_columns()
            .iter()
            .position(|raw| *raw == 15)
            .unwrap();
        let time_visual = table
            .visible_columns()
            .iter()
            .position(|raw| *raw == 16)
            .unwrap();
        assert!(table.sort_visible_column(private_visual, ColumnSort::Descending));
        assert_eq!(table.sort_col, 15);
        assert_eq!(table.row(0).unwrap().pid, 1);
        assert!(table.sort_visible_column(time_visual, ColumnSort::Descending));
        assert_eq!(table.sort_col, 16);
        assert_eq!(table.row(0).unwrap().pid, 2);
        let pid_width = table.columns[1].width;
        let mut widths: Vec<_> = table
            .visible_columns()
            .iter()
            .map(|raw| table.columns[*raw].width)
            .collect();
        widths[private_visual] = px(215.);
        widths[time_visual] = px(193.);
        table.set_column_widths(&widths);
        assert_eq!(table.columns[15].width, px(215.));
        assert_eq!(table.columns[16].width, px(193.));
        assert_eq!(table.columns[1].width, pid_width);
        table.set_hidden_columns(&[]);
        assert_eq!(table.visible_column(15).unwrap().width, px(215.));
        assert_eq!(table.visible_column(16).unwrap().width, px(193.));
        table.set_column_widths(&[px(f32::NAN)]);
        assert!(table.columns[0].width.as_f32().is_finite());
    }
    fn application_fixture(
        reordered: bool,
    ) -> std::sync::Arc<system_monitor_x::monitor::SystemSnapshot> {
        use system_monitor_x::process_groups::{
            ProcessIdentity, VisibleWindow, WindowSnapshot, build_process_groups,
        };
        let mut processes = vec![
            ProcessSample {
                pid: 11,
                name: "app-root.exe".into(),
                start_time_windows_ticks: Some(100),
                session_id: Some(1),
                user_name: Some("Alice".into()),
                cpu_usage: 1.0,
                memory_bytes: 100,
                exe: Some(std::path::PathBuf::from(r"C:\App.exe")),
                ..Default::default()
            },
            ProcessSample {
                pid: 12,
                name: "app-child.exe".into(),
                parent_pid: Some(11),
                start_time_windows_ticks: Some(200),
                session_id: Some(1),
                user_name: Some("Alice".into()),
                cpu_usage: 40.0,
                memory_bytes: 900,
                exe: Some(std::path::PathBuf::from(r"C:\App.exe")),
                ..Default::default()
            },
            ProcessSample {
                pid: 13,
                name: "unclassified.exe".into(),
                start_time_windows_ticks: None,
                cpu_usage: 25.0,
                memory_bytes: 500,
                ..Default::default()
            },
        ];
        let windows = WindowSnapshot {
            available: true,
            windows: vec![VisibleWindow {
                owner: ProcessIdentity::from_sample(&processes[0]).unwrap(),
                title: Some("owned sample title".into()),
                title_may_be_truncated: false,
            }],
            ..Default::default()
        };
        if reordered {
            processes.reverse();
        }
        let application_groups = build_process_groups(&processes, &windows);
        std::sync::Arc::new(system_monitor_x::monitor::SystemSnapshot {
            cpu_ready: true,
            processes,
            application_groups,
            ..Default::default()
        })
    }

    #[test]
    fn applications_fold_by_exact_key_sort_by_aggregate_and_keep_raw_selection() {
        let mut table = ProcessTable::new();
        table.sort_col = 2;
        table.descending = true;
        table.set_snapshot(application_fixture(false));
        assert!(!table.group_mode);
        assert_eq!(table.rows.len(), 3);
        table.group_mode = true;
        table.rebuild();
        assert_eq!(table.rows, vec![0, 2]); // Group CPU=41 outranks raw CPU=25.
        assert_eq!(table.row(0).unwrap().pid, 11);
        table.selected = Some((11, 100));
        assert_eq!(
            table.selected_application_targets(),
            Some(vec![(11, 100), (12, 200)])
        );
        assert!(table.selected_application_unavailable_reason().is_none());
        let key = table.application_groups()[0].key.clone();
        table.toggle_application_group(&key);
        assert_eq!(table.rows, vec![0, 1, 2]);
        table.selected = Some((12, 200));
        assert_eq!(table.selection().unwrap().pid, 12);
        assert!(table.selected_application_targets().is_none());
        table.toggle_application_group(&key);
        assert!(table.selected.is_none()); // Folded child never selects its parent.
        assert_eq!(table.rows, vec![0, 2]);
        assert_eq!(
            table.visible_column(4).unwrap().name.to_string(),
            "GPU（成员最大）"
        );
    }

    #[test]
    fn search_details_and_tree_modes_always_use_raw_process_selection() {
        let mut table = ProcessTable::new();
        table.set_snapshot(application_fixture(false));
        table.group_mode = true;
        table.query = "app-child".into();
        table.rebuild();
        assert_eq!(table.rows, vec![1]);
        table.selected = Some((12, 200));
        assert!(table.selected_application_targets().is_none());
        table.query.clear();
        table.detailed = true;
        table.rebuild();
        assert_eq!(table.rows.len(), 3);
        assert!(table.group_headers.is_empty());
        table.detailed = false;
        table.tree_mode = true;
        table.rebuild();
        assert_eq!(table.rows.len(), 3);
        assert!(table.group_headers.is_empty());
        assert!(table.selected_application_targets().is_none());
    }

    #[test]
    fn application_expansion_survives_reorder_and_rejects_pid_reuse() {
        let mut table = ProcessTable::new();
        table.set_snapshot(application_fixture(false));
        table.group_mode = true;
        table.rebuild();
        let key = table.application_groups()[0].key.clone();
        table.toggle_application_group(&key);
        table.selected = Some((11, 100));
        table.set_snapshot(application_fixture(true));
        assert_eq!(table.rows, vec![2, 1, 0]);
        assert_eq!(table.selected_index(), Some(0));
        assert_eq!(
            table.selected_application_targets(),
            Some(vec![(11, 100), (12, 200)])
        );
        let mut reused = (*application_fixture(false)).clone();
        reused.processes[0].start_time_windows_ticks = Some(300);
        reused.application_groups = system_monitor_x::process_groups::build_process_groups(
            &reused.processes,
            &Default::default(),
        );
        table.set_snapshot(std::sync::Arc::new(reused));
        assert!(table.selected.is_none());
        assert_eq!(table.rows.len(), 3);
        assert!(!table.expanded_groups.contains(&key));
    }

    #[test]
    fn stale_member_identity_refuses_bulk_targets_and_malformed_metadata_keeps_rows() {
        let mut snapshot = (*application_fixture(false)).clone();
        snapshot.processes[1].start_time_windows_ticks = None;
        let mut table = ProcessTable::new();
        table.set_snapshot(std::sync::Arc::new(snapshot));
        table.group_mode = true;
        table.rebuild();
        table.selected = Some((11, 100));
        assert!(table.selected_application_targets().is_none());
        assert!(table.selected_application_unavailable_reason().is_some());
        let mut malformed = (*application_fixture(false)).clone();
        malformed.application_groups[0].indices = vec![0, 99];
        table.set_snapshot(std::sync::Arc::new(malformed));
        assert_eq!(table.rows.len(), 3);
        assert!(table.group_headers.is_empty());
        assert!(table.selected_application_targets().is_none());
    }

    fn named_process(pid: u32, birth: u64, name: &str, cpu: f32) -> ProcessSample {
        ProcessSample {
            pid,
            name: name.into(),
            start_time_windows_ticks: Some(birth),
            cpu_usage: cpu,
            ..Default::default()
        }
    }

    #[test]
    fn names_are_default_and_prefix_case_pid_and_birth_have_stable_order() {
        let mut table = ProcessTable::new();
        table.all = vec![
            named_process(40, 400, "zeta.exe", 90.),
            named_process(9, 200, "alpha.exe", 2.),
            named_process(2, 100, "ALPHA.exe", 99.),
            named_process(5, 500, "alpha-helper.exe", 4.),
            named_process(2, 90, "Alpha.exe", 1.),
            named_process(10, 600, "beta.exe", 0.),
        ];
        table.rebuild();
        assert_eq!((table.sort_col, table.descending), (0, false));
        assert_eq!(table.rows, vec![3, 4, 2, 1, 5, 0]);
        table.descending = true;
        table.rebuild();
        assert_eq!(table.rows, vec![0, 5, 4, 2, 1, 3]);
    }

    #[test]
    fn unchanged_names_reuse_order_but_metrics_are_live_and_pid_reuse_clears_selection() {
        use std::sync::Arc;
        use system_monitor_x::monitor::SystemSnapshot;
        let mut table = ProcessTable::new();
        table.set_snapshot(Arc::new(SystemSnapshot {
            processes: vec![
                named_process(8, 100, "beta.exe", 90.),
                named_process(7, 99, "Alpha.exe", 1.),
            ],
            ..Default::default()
        }));
        assert!(!table.name_order_reused());
        let sorts = table.name_sort_count();
        table.selected = Some((7, 99));
        table.set_snapshot(Arc::new(SystemSnapshot {
            processes: vec![
                named_process(7, 99, "ALPHA.exe", 97.),
                named_process(8, 100, "beta.exe", 0.),
            ],
            ..Default::default()
        }));
        assert!(table.name_order_reused());
        assert_eq!(table.name_sort_count(), sorts);
        assert_eq!(table.rows, vec![0, 1]);
        assert_eq!(table.row(0).unwrap().cpu_usage, 97.);
        assert_eq!(table.selected_index(), Some(0));
        table.set_snapshot(Arc::new(SystemSnapshot {
            processes: vec![
                named_process(7, 101, "alpha.exe", 20.),
                named_process(8, 100, "beta.exe", 99.),
            ],
            ..Default::default()
        }));
        assert!(!table.name_order_reused());
        assert_eq!(table.name_sort_count(), sorts + 1);
        assert!(table.selected.is_none());
        assert_eq!(table.row(0).unwrap().pid, 7);
    }

    #[test]
    fn typed_pid_parent_name_prefix_and_path_queries_use_distinct_sources() {
        let mut table = ProcessTable::new();
        let mut first = named_process(12, 100, "alpha.exe", 1.);
        first.parent_pid = Some(50);
        first.exe = Some(std::path::PathBuf::from(r"C:\Tools\alpha.exe"));
        let mut second = named_process(120, 200, "beta-alpha.exe", 90.);
        second.parent_pid = Some(12);
        second.exe = Some(std::path::PathBuf::from(r"D:\Tools\Beta.exe"));
        table.all = vec![first, second, named_process(3, 300, "pid:12.exe", 0.)];
        for (query, expected) in [
            ("PID:12", vec![12]),
            ("pid:120", vec![120]),
            ("ppid:12", vec![120]),
            ("NAME:alpha", vec![12]),
            ("path:d:\\tools", vec![120]),
            ("alpha", vec![12, 120]),
            ("12", vec![12, 120, 3]),
            ("pid:1x", vec![]),
            ("pid:4294967296", vec![]),
        ] {
            table.query = query.into();
            table.rebuild();
            let actual: Vec<_> = table
                .rows
                .iter()
                .map(|index| table.samples()[*index].pid)
                .collect();
            assert_eq!(actual, expected, "query={query}");
        }
    }

    #[test]
    fn clicked_context_target_is_captured_before_selection_changes_or_pid_reuse() {
        let mut table = ProcessTable::new();
        table.set_snapshot(application_fixture(false));
        table.group_mode = true;
        table.rebuild();
        let target = table.action_target(0).unwrap();
        assert_eq!(
            (target.pid, target.creation_time_windows_ticks),
            (11, Some(100))
        );
        assert_eq!(target.application_targets, Some(vec![(11, 100), (12, 200)]));
        table.selected = Some((13, 0));
        let mut replacement = (*application_fixture(false)).clone();
        replacement.processes[0].start_time_windows_ticks = Some(300);
        replacement.application_groups.clear();
        table.set_snapshot(std::sync::Arc::new(replacement));
        assert_eq!(target.creation_time_windows_ticks, Some(100));
        assert_eq!(target.application_targets, Some(vec![(11, 100), (12, 200)]));
        let unknown_index = table
            .rows
            .iter()
            .position(|index| table.samples()[*index].pid == 13)
            .unwrap();
        let unknown = table.action_target(unknown_index).unwrap();
        assert!(unknown.creation_time_windows_ticks.is_none());
        assert!(unknown.application_targets.is_none());
        assert!(table.action_target(usize::MAX).is_none());
    }

    #[test]
    fn text_tables_default_to_shared_name_order_with_stable_numeric_identity() {
        let mut table = super::TextTable::new(&[("Name", 200.), ("PID", 80.), ("CPU", 80.)])
            .numeric_columns(&[1, 2])
            .stable_identity_columns(&[1]);
        table.all = vec![
            vec!["Zulu".into(), "1".into(), "0".into()],
            vec!["Alpha".into(), "10".into(), "90".into()],
            vec!["ALPHA".into(), "2".into(), "1".into()],
            vec!["Alpha-helper".into(), "9".into(), "60".into()],
        ];
        table.rebuild();
        let identities = |table: &super::TextTable| {
            table
                .rows
                .iter()
                .map(|row| row[1].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(identities(&table), vec!["2", "10", "9", "1"]);
        table.all.reverse();
        for row in &mut table.all {
            row[2] = if row[1] == "2" {
                "100".into()
            } else {
                "0".into()
            };
        }
        table.rebuild();
        assert_eq!(identities(&table), vec!["2", "10", "9", "1"]);
        table.query = "aLpHa".into();
        table.rebuild();
        assert_eq!(identities(&table), vec!["2", "10", "9"]);
        table.sort = Some((2, true));
        table.rebuild();
        assert_eq!(identities(&table), vec!["2", "9", "10"]);
    }

    #[test]
    fn group_headers_default_to_names_even_when_aggregate_cpu_is_larger() {
        let mut snapshot = (*application_fixture(false)).clone();
        snapshot.processes[2].name = "AAA raw.exe".into();
        let mut table = ProcessTable::new();
        table.set_snapshot(std::sync::Arc::new(snapshot));
        table.group_mode = true;
        table.rebuild();
        assert_eq!(table.row(0).unwrap().pid, 13);
        assert_eq!(table.row(1).unwrap().pid, 11);
        assert!(
            table
                .action_target(0)
                .unwrap()
                .application_targets
                .is_none()
        );
        assert_eq!(
            table.action_target(1).unwrap().application_targets,
            Some(vec![(11, 100), (12, 200)])
        );
    }

    #[test]
    fn process_menu_refuses_self_and_core_pids_even_with_exact_birth() {
        for pid in [0, 4, std::process::id()] {
            let target = super::ProcessRowTarget {
                pid,
                creation_time_windows_ticks: Some(116_444_736_000_000_123),
                name: "protected.exe".into(),
                exe: None,
                application_targets: None,
            };
            assert!(!target.can_end_process(), "protected PID={pid}");
        }
        let mut target = super::ProcessRowTarget {
            pid: if std::process::id() == 987_654 {
                987_653
            } else {
                987_654
            },
            creation_time_windows_ticks: Some(116_444_736_000_000_123),
            name: "normal.exe".into(),
            exe: None,
            application_targets: None,
        };
        assert!(target.can_end_process());
        target.creation_time_windows_ticks = None;
        assert!(!target.can_end_process());
        target.creation_time_windows_ticks = Some(0);
        assert!(!target.can_end_process());
    }

    #[test]
    fn application_menu_refuses_any_protected_or_unverified_captured_member() {
        let first = if std::process::id() == 987_654 {
            987_653
        } else {
            987_654
        };
        let second = if std::process::id() == 987_655 {
            987_656
        } else {
            987_655
        };
        let birth = 116_444_736_000_000_123;
        let mut target = super::ProcessRowTarget {
            pid: first,
            creation_time_windows_ticks: Some(birth),
            name: "owned-app.exe".into(),
            exe: None,
            application_targets: Some(vec![(first, birth), (second, birth + 1)]),
        };
        assert!(target.can_end_application());
        for protected in [0, 1, 2, 3, 4, std::process::id()] {
            target.application_targets = Some(vec![(first, birth), (protected, birth)]);
            assert!(
                !target.can_end_application(),
                "protected member={protected}"
            );
        }
        for invalid_ticks in [0, 123, 116_444_736_000_000_000 - 1] {
            target.application_targets = Some(vec![(first, birth), (second, invalid_ticks)]);
            assert!(!target.can_end_application());
        }
        target.application_targets = Some(vec![(first, birth)]);
        assert!(!target.can_end_application());
        target.application_targets = None;
        assert!(!target.can_end_application());
    }
}
