//! Virtualized developer port table. No endpoint/owner queries run during render.
use super::localization::{diagnostic, tr, trs};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    menu::{PopupMenu, PopupMenuItem},
    table::*,
    *,
};
use gpui_kit::*;
use std::{cmp::Ordering, sync::Arc};
use system_monitor_x::{
    name_order::name_key,
    network_ports::{PortEntry, PortKey, PortQuery, PortSearchText, PortSnapshot},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortMenuAction {
    CopyPid,
    CopyLocal,
    CopyRemote,
    CopyProcessPath,
    ShowProcess,
    EndProcess,
}
#[derive(Clone, Debug)]
pub struct PortMenuEvent {
    pub action: PortMenuAction,
    pub entry: PortEntry,
}

pub struct PortsTable {
    snapshot: Arc<PortSnapshot>,
    names: Vec<String>,
    search_text: Vec<PortSearchText>,
    ordered: Vec<usize>,
    pub rows: Vec<usize>,
    pub query: String,
    pub selected: Option<PortKey>,
    columns: Vec<Column>,
    sort_column: usize,
    descending: bool,
}
impl Default for PortsTable {
    fn default() -> Self {
        Self::new()
    }
}
impl PortsTable {
    pub fn new() -> Self {
        let columns = [
            ("name", "进程名称", 240.),
            ("pid", "PID", 78.),
            ("protocol", "协议", 70.),
            ("family", "地址族", 78.),
            ("local_ip", "本地地址", 178.),
            ("local_port", "本地端口", 92.),
            ("remote_ip", "远端地址", 178.),
            ("remote_port", "远端端口", 92.),
            ("state", "TCP 状态", 128.),
            ("identity", "进程身份", 150.),
        ]
        .into_iter()
        .map(|(id, label, width)| Column::new(id, label).width(width).sortable())
        .collect();
        Self {
            snapshot: Arc::new(PortSnapshot::default()),
            names: vec![],
            search_text: vec![],
            ordered: vec![],
            rows: vec![],
            query: String::new(),
            selected: None,
            columns,
            sort_column: 0,
            descending: false,
        }
    }
    pub fn set_snapshot(&mut self, snapshot: Arc<PortSnapshot>) {
        self.snapshot = snapshot;
        self.names = self
            .snapshot
            .entries
            .iter()
            .map(|entry| name_key(entry.process_name.as_deref().unwrap_or("")))
            .collect();
        self.search_text = self
            .snapshot
            .entries
            .iter()
            .map(PortSearchText::new)
            .collect();
        self.sort_order();
        self.rebuild();
    }
    pub fn snapshot(&self) -> &Arc<PortSnapshot> {
        &self.snapshot
    }
    pub fn set_query(&mut self, query: &str) {
        if self.query == query {
            return;
        }
        self.query = query.to_owned();
        self.rebuild();
    }
    /// Filters only the cached order. Sorting is paid on refresh/header change,
    /// not on each search character; no endpoint strings are cloned per query.
    pub fn rebuild(&mut self) {
        let query = PortQuery::new(&self.query);
        self.rows = self
            .ordered
            .iter()
            .copied()
            .filter(|index| {
                query.matches(&self.snapshot.entries[*index], &self.search_text[*index])
            })
            .collect();
        if self.selected_index().is_none() {
            self.selected = None;
        }
    }
    pub fn row(&self, visible: usize) -> Option<&PortEntry> {
        self.snapshot.entries.get(*self.rows.get(visible)?)
    }
    pub fn selection_key(&self, visible: usize) -> Option<PortKey> {
        self.row(visible).map(PortEntry::key)
    }
    pub fn index_for_entry(&self, entry: &PortEntry) -> Option<usize> {
        let key = entry.key();
        self.rows
            .iter()
            .position(|index| self.snapshot.entries[*index].key() == key)
    }
    pub fn selected_index(&self) -> Option<usize> {
        let key = self.selected.as_ref()?;
        self.rows
            .iter()
            .position(|index| self.snapshot.entries[*index].key() == *key)
    }
    pub fn selected_entry(&self) -> Option<&PortEntry> {
        self.row(self.selected_index()?)
    }
    pub fn selected_identity(&self) -> Option<(u32, u64)> {
        self.selected_entry()?.process_identity()
    }
    pub fn select_row(&mut self, visible: Option<usize>) {
        self.selected = visible.and_then(|row| self.selection_key(row));
    }
    pub fn set_column_widths(&mut self, widths: &[Pixels]) {
        for (column, width) in self.columns.iter_mut().zip(widths) {
            if width.as_f32().is_finite() {
                column.width = (*width).clamp(column.min_width, column.max_width);
            }
        }
    }
    fn sort_order(&mut self) {
        let mut ordered: Vec<_> = (0..self.snapshot.entries.len()).collect();
        ordered.sort_by(|left, right| {
            let a = &self.snapshot.entries[*left];
            let b = &self.snapshot.entries[*right];
            let name = self.names[*left].cmp(&self.names[*right]);
            let primary = match self.sort_column {
                0 => a
                    .process_name
                    .is_none()
                    .cmp(&b.process_name.is_none())
                    .then(name),
                1 => a.pid.cmp(&b.pid),
                2 => a.protocol.cmp(&b.protocol),
                3 => a.family.cmp(&b.family),
                4 => a.local.ip().cmp(&b.local.ip()),
                5 => a.local.port().cmp(&b.local.port()),
                6 => a
                    .remote
                    .map(|remote| remote.ip())
                    .cmp(&b.remote.map(|remote| remote.ip())),
                7 => a
                    .remote
                    .map(|remote| remote.port())
                    .cmp(&b.remote.map(|remote| remote.port())),
                8 => a.tcp_state.cmp(&b.tcp_state),
                9 => a.creation_ticks.is_none().cmp(&b.creation_ticks.is_none()),
                _ => Ordering::Equal,
            };
            (if self.descending {
                primary.reverse()
            } else {
                primary
            })
            .then_with(|| self.names[*left].cmp(&self.names[*right]))
            .then(a.pid.cmp(&b.pid))
            .then(a.protocol.cmp(&b.protocol))
            .then(a.family.cmp(&b.family))
            .then(a.local.port().cmp(&b.local.port()))
            .then(a.local.cmp(&b.local))
            .then(a.remote.cmp(&b.remote))
            .then(a.creation_ticks.cmp(&b.creation_ticks))
        });
        self.ordered = ordered;
    }
    fn sort_by_column(&mut self, column: usize, descending: bool) {
        if column >= self.columns.len() {
            return;
        }
        self.sort_column = column;
        self.descending = descending;
        self.sort_order();
        self.rebuild();
    }
}

pub fn sync_selection(
    table: &mut TableState<PortsTable>,
    cx: &mut Context<TableState<PortsTable>>,
) {
    let selected = table.delegate().selected_index();
    if selected != table.selected_row() {
        if let Some(row) = selected {
            table.set_selected_row(row, cx);
        } else {
            table.clear_selection(cx);
        }
    }
}
impl EventEmitter<PortMenuEvent> for TableState<PortsTable> {}
impl TableDelegate for PortsTable {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, index: usize, _: &App) -> Column {
        let mut column = self.columns[index].clone();
        column.name = tr(&column.name);
        column.sort = Some(if index != self.sort_column {
            ColumnSort::Default
        } else if self.descending {
            ColumnSort::Descending
        } else {
            ColumnSort::Ascending
        });
        column
    }
    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(entry) = self.row(row) else {
            return div().into_any_element();
        };
        let value = match column {
            0 => entry
                .process_name
                .clone()
                .unwrap_or_else(|| trs("无法读取名称")),
            1 => entry.pid.to_string(),
            2 => entry.protocol.to_string(),
            3 => entry.family.to_string(),
            4 => address_text(entry.local),
            5 => entry.local.port().to_string(),
            6 => entry.remote.map(address_text).unwrap_or_else(|| "—".into()),
            7 => entry
                .remote
                .map(|remote| remote.port().to_string())
                .unwrap_or_else(|| "—".into()),
            8 => entry
                .tcp_state
                .map(|state| state.to_string())
                .unwrap_or_else(|| "—".into()),
            9 => {
                if entry.creation_ticks.is_some() {
                    trs("已核验创建时间")
                } else {
                    trs("不可验证 / 禁止结束")
                }
            }
            _ => String::new(),
        };
        div()
            .px_2()
            .text_sm()
            .truncate()
            .text_color(if column == 0 {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            })
            .child(value)
            .into_any_element()
    }
    fn perform_sort(
        &mut self,
        column: usize,
        sort: ColumnSort,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.sort_by_column(column, matches!(sort, ColumnSort::Descending));
        cx.defer_in(window, |table, _, cx| sync_selection(table, cx));
        cx.notify();
    }
    fn context_menu(
        &mut self,
        row: usize,
        mut menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(entry) = self.row(row).cloned() else {
            return menu;
        };
        let emitter = cx.entity().downgrade();
        let choices = [
            ("复制 PID", PortMenuAction::CopyPid, false),
            ("复制本地端点", PortMenuAction::CopyLocal, false),
            (
                "复制远端端点",
                PortMenuAction::CopyRemote,
                entry.remote.is_none(),
            ),
            (
                "复制进程路径",
                PortMenuAction::CopyProcessPath,
                entry.exe.is_none(),
            ),
            (
                "在进程页查看",
                PortMenuAction::ShowProcess,
                entry.process_identity().is_none() || entry.pid == 0,
            ),
            (
                "结束占用进程…",
                PortMenuAction::EndProcess,
                !entry.can_end_process(),
            ),
        ];
        for (label, action, disabled) in choices {
            let emitter = emitter.clone();
            let target = entry.clone();
            let id = match action {
                PortMenuAction::CopyPid => "port-copy-pid",
                PortMenuAction::CopyLocal => "port-copy-local",
                PortMenuAction::CopyRemote => "port-copy-remote",
                PortMenuAction::CopyProcessPath => "port-copy-path",
                PortMenuAction::ShowProcess => "port-show-process",
                PortMenuAction::EndProcess => "port-end",
            };
            menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    div()
                        .id(id)
                        .w_full()
                        .text_sm()
                        .child(tr(label))
                        .test_support()
                })
                .disabled(disabled)
                .on_click(move |_, _, cx| {
                    let _ = emitter.update(cx, |_, cx| {
                        cx.emit(PortMenuEvent {
                            action,
                            entry: target.clone(),
                        })
                    });
                }),
            );
        }
        if let Some(note) = entry.identity_note {
            menu = menu.label(diagnostic(&note));
        }
        menu
    }
    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_8()
            .text_color(cx.theme().muted_foreground)
            .child(tr("没有匹配的端口端点；可尝试 port:8080 或 pid:123"))
    }
}
fn address_text(endpoint: std::net::SocketAddr) -> String {
    match endpoint {
        std::net::SocketAddr::V6(endpoint) if endpoint.scope_id() != 0 => {
            format!("{}%{}", endpoint.ip(), endpoint.scope_id())
        }
        endpoint => endpoint.ip().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::PortsTable;
    use std::sync::Arc;
    use system_monitor_x::network_ports::{IpFamily, PortProtocol, TcpState};
    use system_monitor_x::network_ports::{PortEntry, PortSnapshot};
    fn entry(name: &str, pid: u32, ticks: u64, port: u16) -> PortEntry {
        PortEntry {
            pid,
            creation_ticks: Some(ticks),
            process_name: Some(name.into()),
            exe: None,
            protocol: PortProtocol::Tcp,
            family: IpFamily::V4,
            local: format!("127.0.0.1:{port}").parse().unwrap(),
            remote: None,
            tcp_state: Some(TcpState::Listen),
            identity_note: None,
        }
    }
    fn snapshot(entries: Vec<PortEntry>) -> Arc<PortSnapshot> {
        Arc::new(PortSnapshot {
            entries,
            complete: true,
            ..Default::default()
        })
    }
    #[test]
    fn default_name_sort_uses_shared_key_and_numeric_pid_port_tiebreaks() {
        let mut table = PortsTable::new();
        table.set_snapshot(snapshot(vec![
            entry("z.exe", 1, 11, 3),
            entry("A.exe", 20, 22, 2),
            entry("a.EXE", 3, 33, 100),
        ]));
        assert_eq!(
            table
                .rows
                .iter()
                .map(|row| table.snapshot.entries[*row].pid)
                .collect::<Vec<_>>(),
            vec![3, 20, 1]
        );
        table.sort_by_column(5, false);
        assert_eq!(table.row(0).unwrap().local.port(), 2);
    }
    #[test]
    fn refresh_sort_filter_and_tcp_state_change_preserve_exact_endpoint_selection() {
        let selected = entry("a.exe", 100, 1000, 8080);
        let mut table = PortsTable::new();
        table.set_snapshot(snapshot(vec![entry("z.exe", 4, 44, 443), selected.clone()]));
        table.select_row(Some(0));
        let mut changed = selected.clone();
        changed.tcp_state = Some(TcpState::Established);
        table.set_snapshot(snapshot(vec![changed, entry("0.exe", 3, 33, 80)]));
        assert_eq!(table.selected_index(), Some(1));
        assert_eq!(table.selected_identity(), Some((100, 1000)));
        assert_eq!(table.index_for_entry(&selected), Some(1));
        table.set_query("port:443");
        assert_eq!(table.selected_index(), None);
        table.set_query("");
        table.select_row(table.index_for_entry(&selected));
        let mut reused = selected;
        reused.creation_ticks = Some(1001);
        table.set_snapshot(snapshot(vec![reused]));
        assert_eq!(table.selected_identity(), None);
    }
    #[test]
    fn query_filters_thousands_of_model_rows_without_cloning_the_snapshot() {
        let original = snapshot(
            (0..10_000)
                .map(|index| {
                    entry(
                        "owned.exe",
                        index + 10,
                        index as u64 + 1000,
                        (index % 1000) as u16,
                    )
                })
                .collect(),
        );
        let mut table = PortsTable::new();
        table.set_snapshot(original.clone());
        table.set_query("tcp port:808");
        assert_eq!(table.rows.len(), 10);
        assert!(Arc::ptr_eq(table.snapshot(), &original));
        assert_eq!(table.row(0).unwrap().local.port(), 808);
    }
}
