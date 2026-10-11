use gpui_kit::component::{table::*, *};
use gpui_kit::{prelude::FluentBuilder as _, *};
use system_monitor_x::monitor::ProcessSample;

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
    pub rows: Vec<ProcessSample>,
    pub query: String,
    pub sort_col: usize,
    pub descending: bool,
    pub selected: Option<(u32, u64)>,
    pub ready: bool,
    pub memory_total: u64,
    columns: Vec<Column>,
}
impl ProcessTable {
    pub fn new() -> Self {
        Self {
            all: vec![],
            rows: vec![],
            query: String::new(),
            sort_col: 2,
            descending: true,
            selected: None,
            ready: false,
            memory_total: 0,
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
                Column::new("parent", "父进程 PID").width(100.).sortable(),
                Column::new("status", "状态").width(100.).sortable(),
            ],
        }
    }
    pub fn rebuild(&mut self) {
        let query = self.query.to_lowercase();
        self.rows = self
            .all
            .iter()
            .filter(|p| {
                query.is_empty()
                    || p.name.to_lowercase().contains(&query)
                    || p.pid.to_string().contains(&query)
                    || p.exe
                        .as_ref()
                        .is_some_and(|s| s.to_string_lossy().to_lowercase().contains(&query))
            })
            .cloned()
            .collect();
        let (col, descending) = (self.sort_col, self.descending);
        self.rows.sort_by(|a, b| {
            let order = match col {
                0 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                1 => a.pid.cmp(&b.pid),
                2 => a.cpu_usage.total_cmp(&b.cpu_usage),
                3 => a.memory_bytes.cmp(&b.memory_bytes),
                4 => compare_optional(a.gpu_usage, b.gpu_usage),
                5 => a.gpu_memory_bytes.cmp(&b.gpu_memory_bytes),
                6 => compare_optional(a.disk_read_bytes_per_sec, b.disk_read_bytes_per_sec),
                7 => compare_optional(a.disk_write_bytes_per_sec, b.disk_write_bytes_per_sec),
                8 => a.parent_pid.cmp(&b.parent_pid),
                _ => a.status.cmp(&b.status),
            };
            let order = if descending { order.reverse() } else { order };
            order.then(a.pid.cmp(&b.pid))
        });
        if self.selected.is_some_and(|id| {
            !self
                .rows
                .iter()
                .any(|p| (p.pid, p.start_time_unix_secs) == id)
        }) {
            self.selected = None;
        }
    }
    pub fn selection(&self) -> Option<&ProcessSample> {
        self.selected.and_then(|id| {
            self.rows
                .iter()
                .find(|p| (p.pid, p.start_time_unix_secs) == id)
        })
    }
    pub fn selected_index(&self) -> Option<usize> {
        self.selected.and_then(|id| {
            self.rows
                .iter()
                .position(|p| (p.pid, p.start_time_unix_secs) == id)
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
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let p = &self.rows[row];
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
            8 => p
                .parent_pid
                .map(|id| id.to_string())
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
    }
    fn perform_sort(
        &mut self,
        col: usize,
        sort: ColumnSort,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.sort_col = col;
        self.descending = matches!(sort, ColumnSort::Descending);
        self.rebuild();
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
}
impl TextTable {
    pub fn new(columns: &[(&'static str, f32)]) -> Self {
        Self {
            all: vec![],
            rows: vec![],
            query: String::new(),
            sort: None,
            columns: columns
                .iter()
                .enumerate()
                .map(|(i, (s, w))| Column::new(format!("col{i}"), *s).width(*w).sortable())
                .collect(),
        }
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
            self.rows.sort_by(|a, b| {
                if descending {
                    b[column].cmp(&a[column])
                } else {
                    a[column].cmp(&b[column])
                }
            });
        }
    }
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
        assert_eq!(t.rows[0].pid, 400);
        t.query = "app-123".into();
        t.rebuild();
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0].pid, 123);
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
}
