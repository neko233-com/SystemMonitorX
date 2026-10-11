use super::*;

fn optional_text(value: &Option<String>) -> String {
    value
        .clone()
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "—".into())
}

impl SystemMonitorApp {
    pub(super) fn refresh_process_details(&mut self, pid: u32, ticks: u64, cx: &mut Context<Self>) {
        let identity = (pid, ticks);
        if self.process_details_loading && self.process_details_identity == Some(identity) {
            return;
        }
        if self.process_details_identity != Some(identity) {
            self.process_details = None;
        }
        self.process_details_identity = Some(identity);
        self.process_details_loading = true;
        self.process_details_error = None;
        self.process_details_generation = self.process_details_generation.wrapping_add(1);
        let generation = self.process_details_generation;
        cx.spawn(async move |view, cx| {
            let result =
                smol::unblock(move || ProcessDetails::read(pid, ticks).map(Arc::new)).await;
            let _ = view.update(cx, |this, cx| {
                // A -> B -> A selection and manual refresh can leave an older
                // read of A in flight. Its result must not replace newer data.
                if this.process_details_identity != Some(identity)
                    || this.process_details_generation != generation
                {
                    return;
                }
                this.process_details_loading = false;
                let still_alive = this.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.processes.iter().any(|process| {
                        process.pid == pid && process.start_time_windows_ticks == Some(ticks)
                    })
                });
                if !still_alive {
                    this.process_details = None;
                    this.process_details_error =
                        Some("所选进程已退出或身份已改变，详情已过期".into());
                } else {
                    match result {
                        Ok(details) => {
                            this.process_details = Some(details);
                            this.process_details_error = None;
                        }
                        Err(error) => {
                            this.process_details = None;
                            this.process_details_error =
                                Some(format!("进程详情读取失败：{error:#}"));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn selected_process_details(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(identity) = self.process_details_identity else {
            return div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("无法取得精确进程身份，详细信息暂不可用")
                .into_any_element();
        };
        let details = self
            .process_details
            .as_deref()
            .filter(|details| details.pid == identity.0 && details.creation_ticks == identity.1);
        let summary = if self.process_details_loading {
            "正在后台读取…".to_owned()
        } else if self.process_details_error.is_some() {
            "部分详情不可用".to_owned()
        } else if let Some(details) = details {
            format!(
                "{} · {}",
                optional_text(&details.architecture),
                optional_text(&details.file_version)
            )
        } else {
            "按所选进程读取".to_owned()
        };
        let expanded = self.process_details_expanded;
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("toggle-process-details")
                            .small()
                            .ghost()
                            .label(if expanded {
                                "收起详情"
                            } else {
                                "进程详情"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.process_details_expanded = !this.process_details_expanded;
                                if this.process_details_expanded
                                    && this.process_details.is_none()
                                    && !this.process_details_loading
                                    && let Some((pid, ticks)) = this.process_details_identity
                                {
                                    this.refresh_process_details(pid, ticks, cx);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(summary),
                    )
                    .when(expanded, |element| {
                        element.child(
                            Button::new("refresh-process-details")
                                .small()
                                .ghost()
                                .label("刷新详情")
                                .disabled(self.process_details_loading)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some((pid, ticks)) = this.process_details_identity {
                                        this.refresh_process_details(pid, ticks, cx);
                                    }
                                })),
                        )
                    }),
            )
            .when(expanded, |element| {
                let mut body = v_flex()
                    .id("selected-process-details-scroll")
                    .w_full()
                    .min_w_0()
                    .max_h(px(190.))
                    .overflow_y_scroll()
                    .gap_1()
                    .text_xs()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_2();
                if let Some(error) = self.process_details_error.as_ref() {
                    body = body.child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(error.clone()),
                    );
                }
                if let Some(details) = details {
                    let rows = [
                        ("命令行", optional_text(&details.command_line)),
                        (
                            "进程 / 系统架构",
                            format!(
                                "{} / {}",
                                optional_text(&details.architecture),
                                optional_text(&details.native_architecture)
                            ),
                        ),
                        ("文件版本", optional_text(&details.file_version)),
                        ("文件描述", optional_text(&details.file_description)),
                        ("公司（文件资源）", optional_text(&details.company_name)),
                        (
                            "产品 / 产品版本",
                            format!(
                                "{} / {}",
                                optional_text(&details.product_name),
                                optional_text(&details.product_version)
                            ),
                        ),
                        ("原始文件名", optional_text(&details.original_filename)),
                        (
                            "读取时优先级 / EcoQoS",
                            format!(
                                "{} / {}",
                                optional_text(&details.priority),
                                details
                                    .efficiency_mode
                                    .map(|enabled| if enabled { "已启用" } else { "未启用" })
                                    .unwrap_or("—")
                            ),
                        ),
                        ("当前目录", optional_text(&details.current_directory)),
                    ];
                    body = body
                        .children(
                            rows.into_iter()
                                .map(|(label, value)| Self::detail_row(label, value, cx)),
                        )
                        .child(div().text_color(cx.theme().muted_foreground).child(
                            details.file_metadata_source.clone().unwrap_or_else(|| {
                                "版本资源未提供；公司文字不能作为已验证的签名发布者".into()
                            }),
                        ))
                        .children(details.issues.iter().map(|issue| {
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(issue.clone())
                        }))
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child("详情在选择进程或点击刷新时读取"),
                        );
                } else if self.process_details_loading {
                    body = body.child("正在后台读取所选进程的详细信息…");
                }
                element.child(body)
            })
            .into_any_element()
    }
}
