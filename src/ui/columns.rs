use super::*;
use gpui_kit::component::{button::Button, checkbox::Checkbox};
use std::rc::Rc;

fn update_column_visibility(
    hidden: &mut Vec<usize>,
    raw: usize,
    visible: bool,
    count: usize,
) -> bool {
    if raw == 0 || raw >= count {
        return false;
    }
    let previous = hidden.clone();
    hidden.retain(|column| *column > 0 && *column < count && *column != raw);
    if !visible {
        hidden.push(raw);
    }
    hidden.sort_unstable();
    hidden.dedup();
    *hidden != previous
}

impl SystemMonitorApp {
    pub(super) fn columns_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let options = self.processes.read(cx).delegate().column_options();
        let detailed = self.processes.read(cx).delegate().detailed;
        let controls: Vec<_> = options
            .into_iter()
            .map(|(raw, name)| {
                let on_change = Rc::new(cx.listener(move |this, visible: &bool, window, cx| {
                    let count = this.processes.read(cx).delegate().raw_column_count();
                    if !update_column_visibility(
                        &mut this.settings.hidden_process_columns,
                        raw,
                        *visible,
                        count,
                    ) {
                        return;
                    }
                    let hidden = this.settings.hidden_process_columns.clone();
                    this.processes.update(cx, |table, cx| {
                        table.delegate_mut().set_hidden_columns(&hidden);
                        table.refresh(cx);
                        tables::sync_selection(table, cx);
                        cx.notify();
                    });
                    if this.settings_writer.send(this.settings.clone()).is_err() {
                        this.message = Some("列显示设置暂时无法保存。".into());
                    }
                    cx.notify();
                    window.refresh();
                }));
                (raw, name, on_change)
            })
            .collect();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let hidden = view
                .upgrade()
                .map(|view| view.read(cx).settings.hidden_process_columns.clone())
                .unwrap_or_default();
            dialog
                .title("选择进程列")
                .width(px(500.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("勾选要显示的列。更改立即保存，进程名称始终显示。"),
                        )
                        .child(
                            v_flex()
                                .id("process-columns-options")
                                .gap_1()
                                .max_h(px(340.))
                                .overflow_y_scroll()
                                .children(controls.iter().map(|(raw, name, on_change)| {
                                    let on_change = on_change.clone();
                                    Checkbox::new(("process-column", *raw))
                                        .label(if *raw == 0 {
                                            format!("{name}（始终显示）")
                                        } else {
                                            name.clone()
                                        })
                                        .checked(*raw == 0 || !hidden.contains(raw))
                                        .disabled(*raw == 0)
                                        .on_change(move |visible, window, cx| {
                                            on_change(visible, window, cx)
                                        })
                                })),
                        )
                        .when(!detailed, |dialog| {
                            dialog.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("线程、句柄、用户等字段可在「详细信息」页面选择。"),
                            )
                        }),
                )
                .footer(
                    h_flex().justify_end().child(
                        Button::new("process-columns-close")
                            .label("关闭")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
                )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::update_column_visibility;

    #[test]
    fn changing_basic_columns_keeps_detail_preferences_and_name_visible() {
        let mut hidden = vec![15, 17];
        assert!(update_column_visibility(&mut hidden, 2, false, 18));
        assert_eq!(hidden, vec![2, 15, 17]);
        assert!(!update_column_visibility(&mut hidden, 0, false, 18));
        assert!(!update_column_visibility(&mut hidden, 18, false, 18));
        assert_eq!(hidden, vec![2, 15, 17]);
        assert!(update_column_visibility(&mut hidden, 2, true, 18));
        assert_eq!(hidden, vec![15, 17]);
        assert!(!update_column_visibility(&mut hidden, 2, true, 18));
    }
}
