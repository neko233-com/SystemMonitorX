use gpui_kit::component::{table::*, *};
use gpui_kit::{prelude::FluentBuilder as _, *};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use system_monitor_x::monitor::{ProcessSample, SystemSnapshot};
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

pub struct ProcessTable {
    pub all: Vec<ProcessSample>,
    /// Visible row indices share the immutable collector snapshot. Filtering
    /// and sorting never clone every process name/path or its new detail fields.
    pub rows: Vec<usize>,
    snapshot: Option<Arc<SystemSnapshot>>,
    names: Vec<String>,
    search_keys: Vec<String>,
    pub query: String,
    pub sort_col: usize,
    pub descending: bool,
    pub selected: Option<(u32, u64)>,
    pub ready: bool,
    pub memory_total: u64,
    pub detailed: bool,
    pub tree_mode: bool,
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
            query: String::new(),
            sort_col: 2,
            descending: true,
            selected: None,
            ready: false,
            memory_total: 0,
            detailed: false,
            tree_mode: false,
            collapsed: HashSet::new(),
            tree_rows: HashMap::new(),
            visible_process_columns: (0..12).collect(),
            visible_detail_columns: (0..18).collect(),
            columns: vec![
                Column::new("name", "进程名称").width(270.).sortable(),
                Column::new("pid", "PID").width(74.).sortable(),
                Column::new("cpu", "CPU")
                    .width(95.)
                    .sortable()
                    .sort(ColumnSort::Descending),
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
    fn prepare_keys(&mut self) {
        self.names = self
            .samples()
            .iter()
            .map(|p| p.name.to_lowercase())
            .collect();
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
    }
    pub fn rebuild(&mut self) {
        // Standalone model tests may replace their owned vector directly.
        if self.snapshot.is_none() {
            self.prepare_keys();
        }
        let query = self.query.to_lowercase();
        let mut rows: Vec<usize> = self
            .search_keys
            .iter()
            .enumerate()
            .filter(|(_, key)| query.is_empty() || key.contains(&query))
            .map(|(index, _)| index)
            .collect();
        let (col, descending) = (self.sort_col, self.descending);
        let samples = self.samples();
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
            order.then(a.pid.cmp(&b.pid))
        });
        self.tree_rows.clear();
        if self.tree_mode && query.is_empty() {
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
                .child(p.name.clone())
                .into_any_element();
        }
        let text = match col {
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
        };
        let intensity = match col {
            2 => p.cpu_usage / 100.,
            3 => (p.memory_bytes as f32 / self.memory_total.max(1) as f32).min(1.),
            4 => p.gpu_usage.unwrap_or(0.) / 100.,
            _ => 0.,
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
}
impl TextTable {
    pub fn new(columns: &[(&'static str, f32)]) -> Self {
        Self {
            all: vec![],
            rows: vec![],
            query: String::new(),
            sort: None,
            numeric: vec![],
            columns: columns
                .iter()
                .enumerate()
                .map(|(i, (s, w))| Column::new(format!("col{i}"), *s).width(*w).sortable())
                .collect(),
        }
    }
    pub fn numeric_columns(mut self, columns: &[usize]) -> Self {
        self.numeric = columns.to_vec();
        self
    }
    pub fn rebuild(&mut self) {
        let q = self.query.to_lowercase();
        self.rows = self
            .all
            .iter()
            .filter(|r| r.iter().any(|s| s.to_lowercase().contains(&q)))
            .cloned()
            .collect();
        if let Some((column, descending)) = self.sort {
            let numeric = self.numeric.contains(&column);
            self.rows.sort_by(|a, b| {
                let order = if numeric {
                    compare_optional(numeric_value(&a[column]), numeric_value(&b[column]))
                } else {
                    a[column].cmp(&b[column])
                };
                if descending { order.reverse() } else { order }
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
        assert_eq!(table.sort_col, 2);
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
}
