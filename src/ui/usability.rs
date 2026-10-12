use super::*;
use system_monitor_x::developer_tools::{SavedSearch, SearchScope};
use system_monitor_x::insights::{MetricValue, ObservedLoadLevel, ResourceKind, SnapshotFreshness};

pub(super) fn deserialize_saved_searches<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<SavedSearch>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    let searches = value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|v| serde_json::from_value::<SavedSearch>(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    Ok(system_monitor_x::developer_tools::normalize_saved_searches(
        searches,
    ))
}
#[derive(Clone, Copy)]
enum QuickItem {
    Page(Page),
    Task,
    Pause,
    Export,
    Help,
}
fn quick_items() -> Vec<(SharedString, &'static str, &'static str, QuickItem)> {
    vec![
        (
            Page::Overview.title(),
            "Ctrl + 1",
            "overview home resources cpu memory",
            QuickItem::Page(Page::Overview),
        ),
        (
            Page::Processes.title(),
            "Ctrl + 2",
            "processes tasks pid",
            QuickItem::Page(Page::Processes),
        ),
        (
            Page::Ports.title(),
            "Ctrl + 3",
            "ports connections tcp udp socket",
            QuickItem::Page(Page::Ports),
        ),
        (
            Page::Performance.title(),
            "",
            "performance cpu memory disk gpu",
            QuickItem::Page(Page::Performance),
        ),
        (
            Page::Details.title(),
            "",
            "details process pid threads handles",
            QuickItem::Page(Page::Details),
        ),
        (
            Page::Startup.title(),
            "",
            "startup launch boot apps",
            QuickItem::Page(Page::Startup),
        ),
        (
            Page::Services.title(),
            "",
            "services windows background",
            QuickItem::Page(Page::Services),
        ),
        (
            Page::Devices.title(),
            "",
            "devices hardware sensors directx gpu",
            QuickItem::Page(Page::Devices),
        ),
        (
            Page::History.title(),
            "",
            "history applications usage",
            QuickItem::Page(Page::History),
        ),
        (
            Page::Users.title(),
            "",
            "users sessions accounts",
            QuickItem::Page(Page::Users),
        ),
        (
            Page::Settings.title(),
            "",
            "settings preferences language updates",
            QuickItem::Page(Page::Settings),
        ),
        (
            tr("运行新任务"),
            "Ctrl + N",
            "run new task launch",
            QuickItem::Task,
        ),
        (
            tr("暂停 / 恢复显示"),
            "Ctrl + P",
            "pause resume refresh",
            QuickItem::Pause,
        ),
        (
            tr("导出 CSV"),
            "Ctrl + E",
            "export csv save",
            QuickItem::Export,
        ),
        (tr("使用帮助"), "F1", "help shortcuts", QuickItem::Help),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SavedQueryError {
    Empty,
    InvalidCharacters,
    TooLong,
}
/// Validate before touching the old list. Display names have a smaller limit
/// than queries, so truncate Unicode characters in the name only.
fn save_query(
    existing: &mut Vec<SavedSearch>,
    scope: SearchScope,
    query: &str,
) -> Result<(), SavedQueryError> {
    let name: String = query.trim().chars().take(60).collect();
    let saved = SavedSearch::new(scope, &name, query).map_err(|_| {
        if query
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
        {
            SavedQueryError::InvalidCharacters
        } else if query.trim().is_empty() {
            SavedQueryError::Empty
        } else {
            SavedQueryError::TooLong
        }
    })?;
    let mut updated = existing.clone();
    updated.retain(|item| !(item.scope == saved.scope && item.query == saved.query));
    updated.insert(0, saved);
    *existing = system_monitor_x::developer_tools::normalize_saved_searches(updated);
    Ok(())
}

fn load_level_text(level: ObservedLoadLevel) -> SharedString {
    match level {
        ObservedLoadLevel::BelowHigh => tr("当前占用"),
        ObservedLoadLevel::High => tr("占用较高"),
        ObservedLoadLevel::VeryHigh => tr("占用很高"),
        ObservedLoadLevel::Unavailable => tr("不可用"),
    }
}
fn freshness_text(freshness: SnapshotFreshness) -> SharedString {
    match freshness {
        SnapshotFreshness::Current => tr("本次资源占用，帮助你决定接下来查看哪里。"),
        SnapshotFreshness::Paused => tr("已暂停：当前显示的是采样快照"),
        SnapshotFreshness::Delayed => tr("采样已延迟：当前数值可能已经过期"),
        SnapshotFreshness::Unknown => tr("采样时间未知：当前显示的是采样快照"),
    }
}
impl SystemMonitorApp {
    pub(super) fn go_overview(&mut self, _: &GoOverview, w: &mut Window, cx: &mut Context<Self>) {
        self.set_page(Page::Overview, w, cx);
    }
    pub(super) fn go_processes(&mut self, _: &GoProcesses, w: &mut Window, cx: &mut Context<Self>) {
        self.set_page(Page::Processes, w, cx);
    }
    pub(super) fn go_ports(&mut self, _: &GoPorts, w: &mut Window, cx: &mut Context<Self>) {
        self.set_page(Page::Ports, w, cx);
    }
    pub(super) fn refresh_page(&mut self, _: &RefreshPage, _: &mut Window, cx: &mut Context<Self>) {
        self.sampler_refresh.store(true, Ordering::Release);
        match self.page {
            Page::Ports => self.refresh_ports(cx),
            Page::Services | Page::Startup => self.refresh_metadata(cx),
            Page::Devices => {
                self.refresh_hardware(cx);
                self.refresh_directx(cx);
            }
            _ => {
                self.refresh_visible_snapshot(cx);
                if let Some((pid, ticks)) = self.process_details_identity {
                    self.refresh_process_details(pid, ticks, cx);
                }
            }
        }
        cx.notify();
    }
    pub(super) fn quick_actions(
        &mut self,
        _: &QuickActions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.quick_search.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder(tr("搜索页面或操作"), window, cx);
        });
        let search = self.quick_search.clone();
        let parent = cx.entity().downgrade();
        let run = std::rc::Rc::new(cx.listener(|this, item: &QuickItem, window, cx| {
            window.close_dialog(cx);
            match *item {
                QuickItem::Page(page) => this.set_page(page, window, cx),
                QuickItem::Task => this.new_task(&NewTask, window, cx),
                QuickItem::Pause => this.toggle_pause(&TogglePause, window, cx),
                QuickItem::Export => this.export_csv(&ExportCsv, window, cx),
                QuickItem::Help => this.show_help(&ShowHelp, window, cx),
            }
        }));
        window.open_dialog(cx, move |dialog, _, cx| {
            let query = search.read(cx).value().trim().to_lowercase();
            let rows: Vec<_> = quick_items()
                .into_iter()
                .filter(|(label, shortcut, keywords, _)| {
                    query.is_empty()
                        || label.to_lowercase().contains(&query)
                        || keywords.contains(&query)
                        || shortcut.to_lowercase().contains(&query)
                })
                .collect();
            let unavailable = parent.upgrade().is_none();
            dialog
                .title(tr("快捷操作"))
                .width(px(540.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(Input::new(&search).prefix(Icon::new(UiIcon::Search).size(px(16.))))
                        .child(
                            v_flex()
                                .id("quick-action-results")
                                .test_support()
                                .max_h(px(350.))
                                .overflow_y_scroll()
                                .gap_1()
                                .when(rows.is_empty(), |e| e.child(tr("没有匹配的操作")))
                                .children(rows.into_iter().enumerate().map(
                                    |(index, (label, shortcut, _, item))| {
                                        let run = run.clone();
                                        Button::new(format!("quick-action-{index}"))
                                            .ghost()
                                            .label(if shortcut.is_empty() {
                                                label
                                            } else {
                                                format!("{label} · {shortcut}").into()
                                            })
                                            .disabled(unavailable)
                                            .on_click(move |_, w, cx| run(&item, w, cx))
                                    },
                                )),
                        ),
                )
                .footer(
                    h_flex().justify_end().child(
                        Button::new("quick-actions-close")
                            .label(tr("关闭"))
                            .on_click(|_, w, cx| w.close_dialog(cx)),
                    ),
                )
        });
        self.quick_search
            .update(cx, |input, cx| input.focus(window, cx));
    }
    pub(super) fn show_help(&mut self, _: &ShowHelp, window: &mut Window, cx: &mut Context<Self>) {
        window.open_dialog(cx, |dialog, _, cx| dialog.title(tr("使用帮助")).width(px(640.)).child(
            v_flex().id("help-content").test_support().max_h(px(380.)).overflow_y_scroll().gap_4()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(tr("先看概览，再看进程")))
            .child(div().child(tr("概览显示本次采样的资源占用与主要使用者。点击进程可继续查看路径、端口和详细信息。")))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(tr("快速查询")))
            .child(Self::detail_row("pid:1234", trs("精确进程 PID"), cx))
            .child(Self::detail_row("name:chrome", trs("进程名称前缀"), cx))
            .child(Self::detail_row("path:project", trs("程序路径包含文字"), cx))
            .child(Self::detail_row("port:3000", trs("本地或远端端口"), cx))
            .child(Self::detail_row("tcp / udp", trs("端口协议"), cx))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(tr("常用快捷键")))
            .children([("Ctrl + Shift + P", tr("快捷操作")), ("Ctrl + F", tr("搜索")), ("Ctrl + P", tr("暂停 / 恢复显示")), ("F5", tr("刷新列表")), ("F1", tr("使用帮助"))].into_iter().map(|(key, label)| Self::detail_row(key, label, cx)))
            .child(div().child(tf("启动参数：{flags}", &[("flags", "--pid / --port / --page / --simple / --developer / --help".to_owned())])))
            .child(div().child(tr("结束进程可能丢失未保存内容。字段显示 — 表示未取得数据；部分信息需要管理员权限或驱动支持。")))
        ).footer(h_flex().justify_end().child(Button::new("help-close").label(tr("关闭")).on_click(|_, w, cx| w.close_dialog(cx)))));
    }
    pub(super) fn set_simple_columns(&mut self, simple: bool, cx: &mut Context<Self>) {
        self.settings.hidden_process_columns = if simple {
            (5..18).collect()
        } else {
            Vec::new()
        };
        let hidden = self.settings.hidden_process_columns.clone();
        self.processes.update(cx, |table, cx| {
            table.delegate_mut().set_hidden_columns(&hidden);
            table.refresh(cx);
            tables::sync_selection(table, cx);
            cx.notify();
        });
        let _ = self.settings_writer.send(self.settings.clone());
        cx.notify();
    }
    pub(super) fn saved_search_strip(&self, cx: &mut Context<Self>) -> AnyElement {
        let scope = if self.page == Page::Ports {
            SearchScope::Ports
        } else {
            SearchScope::Processes
        };
        let can_save = !self.search.read(cx).value().trim().is_empty();
        h_flex()
            .id("saved-searches")
            .test_support()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("save-search")
                    .ghost()
                    .small()
                    .label(tr("保存查询"))
                    .disabled(!can_save)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let query = this.search.read(cx).value().to_string();
                        match save_query(&mut this.settings.saved_searches, scope, &query) {
                            Ok(()) => {
                                let _ = this.settings_writer.send(this.settings.clone());
                                this.message = Some(msg("查询已保存"));
                            }
                            Err(error) => {
                                this.message = Some(match error {
                                    SavedQueryError::Empty => msg("请输入查询后再保存"),
                                    SavedQueryError::InvalidCharacters => {
                                        msg("查询不能包含换行或控制字符")
                                    }
                                    SavedQueryError::TooLong => msg("查询最多支持 256 个字符"),
                                });
                            }
                        }
                        cx.notify();
                    })),
            )
            .children(
                self.settings
                    .saved_searches
                    .iter()
                    .filter(|item| item.scope == scope)
                    .cloned()
                    .enumerate()
                    .map(|(index, item)| {
                        let delete_query = item.query.clone();
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new(format!("saved-query-{index}"))
                                    .ghost()
                                    .small()
                                    .label(item.name)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.search.update(cx, |input, cx| {
                                            input.set_value(item.query.clone(), window, cx)
                                        });
                                        this.filter(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(format!("delete-query-{index}"))
                                    .ghost()
                                    .small()
                                    .icon(UiIcon::X)
                                    .tooltip(trs("删除保存的查询"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.settings.saved_searches.retain(|item| {
                                            !(item.scope == scope && item.query == delete_query)
                                        });
                                        let _ = this.settings_writer.send(this.settings.clone());
                                        cx.notify();
                                    })),
                            )
                    }),
            )
            .into_any_element()
    }
    pub(super) fn overview_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(overview) = self.overview.as_deref() else {
            return div().p_6().child(tr("等待有效采样")).into_any_element();
        };
        let value = |load: &system_monitor_x::insights::ObservedLoad| {
            load.measurement
                .as_ref()
                .map(|e| match e.value {
                    MetricValue::Percent(v) => format!("{v:.1}%"),
                    MetricValue::Bytes(v) => bytes(v),
                    MetricValue::BytesPerSecond(v) => rate(Some(v)),
                })
                .unwrap_or_else(|| "—".into())
        };
        v_flex()
            .id("overview-content")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .gap_6()
            .child(
                v_flex()
                    .gap_1()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .id("overview-freshness")
                            .test_support()
                            .child(freshness_text(overview.freshness)),
                    )
                    .when_some(overview.observed_age.age_millis, |element, age| {
                        element.child(div().id("overview-age").test_support().text_xs().child(tf(
                            "数据更新于 {seconds} 秒前",
                            &[("seconds", format!("{:.0}", age as f64 / 1_000.0))],
                        )))
                    }),
            )
            .child(
                h_flex()
                    .items_start()
                    .flex_wrap()
                    .gap_6()
                    .pb_4()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .children(
                        [
                            (tr("CPU"), &overview.cpu),
                            (tr("内存"), &overview.memory),
                            (tr("磁盘"), &overview.disk),
                            (tr("GPU"), &overview.gpu),
                        ]
                        .into_iter()
                        .map(|(label, load)| {
                            let metric_id = match load.resource {
                                ResourceKind::Cpu => "overview-metric-cpu",
                                ResourceKind::Memory => "overview-metric-memory",
                                ResourceKind::Disk => "overview-metric-disk",
                                ResourceKind::Gpu => "overview-metric-gpu",
                            };
                            v_flex()
                                .id(metric_id)
                                .test_support()
                                .min_w(px(135.))
                                .flex_1()
                                .gap_2()
                                .child(label)
                                .child(
                                    div()
                                        .text_2xl()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(value(load)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(load_level_text(load.level)),
                                )
                                .when_some(
                                    match load.resource {
                                        ResourceKind::Disk => Some(tr("已测卷的最高活动率")),
                                        ResourceKind::Gpu => Some(tr("已测 GPU 的最忙引擎")),
                                        _ => None,
                                    },
                                    |element, note| {
                                        element.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(note),
                                        )
                                    },
                                )
                        }),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_6()
                    .child(self.consumer_list(tr("CPU 占用最多"), &overview.top_cpu, true, cx))
                    .child(self.consumer_list(tr("内存占用最多"), &overview.top_memory, false, cx)),
            )
            .when(!overview.low_disk_space.is_empty(), |element| {
                element.child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(tr("可用磁盘空间较少")),
                        )
                        .children(overview.low_disk_space.iter().map(|disk| {
                            div().child(format!(
                                "{} · {}",
                                disk.mount_point,
                                bytes(disk.available_bytes)
                            ))
                        })),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(
                        [
                            Page::Processes,
                            Page::Performance,
                            Page::Startup,
                            Page::Ports,
                        ]
                        .into_iter()
                        .map(|page| {
                            Button::new(format!("overview-{}", page.id()))
                                .small()
                                .label(page.title())
                                .on_click(
                                    cx.listener(move |this, _, w, cx| this.set_page(page, w, cx)),
                                )
                        }),
                    )
                    .child(
                        Button::new("overview-help")
                            .ghost()
                            .small()
                            .label(tr("使用帮助"))
                            .on_click(
                                cx.listener(|this, _, w, cx| this.show_help(&ShowHelp, w, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(
                        "资源高占用不等于故障。这里反映采样负载，不会自动结束进程或清理内存。",
                    )),
            )
            .into_any_element()
    }
    fn consumer_list(
        &self,
        title: SharedString,
        consumers: &[system_monitor_x::insights::TopConsumer],
        cpu: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .min_w(px(270.))
            .flex_1()
            .gap_3()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
            .when(consumers.is_empty(), |element| {
                element.child(tr("暂无可展示的进程记录"))
            })
            .children(consumers.iter().map(|process| {
                let pid = process.pid;
                let ticks = process.creation_time_windows_ticks;
                Button::new(format!(
                    "overview-consumer-{}-{pid}",
                    if cpu { "cpu" } else { "memory" }
                ))
                .ghost()
                .label(format!(
                    "{} · PID {pid} · {}",
                    process.name,
                    match process.measurement.value {
                        MetricValue::Percent(v) => format!("{v:.1}%"),
                        MetricValue::Bytes(v) => bytes(v),
                        MetricValue::BytesPerSecond(v) => rate(Some(v)),
                    }
                ))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_page(Page::Processes, window, cx);
                    this.search.update(cx, |input, cx| {
                        input.set_value(format!("pid:{pid}"), window, cx)
                    });
                    this.filter(cx);
                    this.processes.update(cx, |table, cx| {
                        table.delegate_mut().selected = None;
                        table.clear_selection(cx);
                    });
                    if let Some(ticks) = ticks {
                        this.select_exact_process(pid, ticks, cx);
                    }
                    cx.notify();
                }))
            }))
            .into_any_element()
    }
    pub(super) fn select_exact_process(&mut self, pid: u32, ticks: u64, cx: &mut Context<Self>) {
        self.processes.update(cx, |table, cx| {
            if let Some(row) = (0..table.delegate().rows.len()).find(|&row| {
                table
                    .delegate()
                    .row(row)
                    .is_some_and(|p| p.pid == pid && p.start_time_windows_ticks == Some(ticks))
            }) {
                table.delegate_mut().selected = Some((pid, ticks));
                table.set_selected_row(row, cx);
                table.scroll_to_row(row, cx);
            } else {
                table.delegate_mut().selected = None;
                table.clear_selection(cx);
            }
        });
        if self.snapshot.as_ref().is_some_and(|s| {
            s.processes
                .iter()
                .any(|p| p.pid == pid && p.start_time_windows_ticks == Some(ticks))
        }) {
            self.refresh_process_details(pid, ticks, cx);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::{SavedQueryError, SavedSearch, SearchScope, Settings, save_query};
    #[test]
    fn malformed_optional_searches_do_not_discard_other_settings() {
        for field in ["null", "17", "{}", "[{},null,17]"] {
            let settings: Settings = serde_json::from_str(&format!(
                r#"{{"interval_ms":2000,"dark":false,"saved_searches":{field}}}"#
            ))
            .unwrap();
            assert_eq!(settings.interval_ms, 2000);
            assert!(!settings.dark);
            assert!(settings.saved_searches.is_empty());
        }
    }
    #[test]
    fn saved_query_truncates_unicode_display_name_without_losing_long_query() {
        let query = format!("path:C:\\{}\\app.exe", "国际-é-日本".repeat(14));
        assert!(query.chars().count() > 60 && query.chars().count() <= 256);
        let mut entries = Vec::new();
        save_query(&mut entries, SearchScope::Processes, &query).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name.chars().count(), 60);
        assert_eq!(entries[0].name, query.chars().take(60).collect::<String>());
        assert_eq!(entries[0].query, query);
        assert_eq!(entries[0].scope, SearchScope::Processes);
        let serialized = serde_json::to_string(&entries).unwrap();
        let restored: Vec<SavedSearch> = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored, entries);
    }

    #[test]
    fn invalid_saved_query_preserves_previous_entries_and_reports_reason() {
        let original = vec![
            SavedSearch::new(SearchScope::Processes, "Editor", "name:code").unwrap(),
            SavedSearch::new(SearchScope::Ports, "Local development", "port:3000").unwrap(),
        ];
        for (query, expected) in [
            ("".to_owned(), SavedQueryError::Empty),
            ("   ".to_owned(), SavedQueryError::Empty),
            (
                "path:folder\nnext".to_owned(),
                SavedQueryError::InvalidCharacters,
            ),
            (
                "pid:1\u{2028}pid:2".to_owned(),
                SavedQueryError::InvalidCharacters,
            ),
            ("a".repeat(257), SavedQueryError::TooLong),
        ] {
            let mut entries = original.clone();
            assert_eq!(
                save_query(&mut entries, SearchScope::Processes, &query),
                Err(expected)
            );
            assert_eq!(entries, original);
        }
    }

    #[test]
    fn saved_query_limit_is_inclusive_and_replacing_a_long_query_keeps_other_scope() {
        let query = format!("path:{}", "国".repeat(251));
        let mut entries = vec![
            SavedSearch::new(SearchScope::Processes, "Existing short name", &query).unwrap(),
            SavedSearch::new(SearchScope::Ports, "Other scope", &query).unwrap(),
        ];
        save_query(&mut entries, SearchScope::Processes, &query).unwrap();
        assert_eq!(entries.len(), 2);
        let saved = entries
            .iter()
            .find(|entry| entry.scope == SearchScope::Processes)
            .unwrap();
        assert_eq!(saved.query.chars().count(), 256);
        assert_eq!(saved.name.chars().count(), 60);
        assert_eq!(saved.query, query);
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.scope == SearchScope::Ports)
                .unwrap()
                .name,
            "Other scope"
        );
    }
}
