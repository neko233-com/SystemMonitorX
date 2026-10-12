use super::*;

fn optional_text(value: &Option<String>) -> String {
    value
        .clone()
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "—".into())
}

// Architecture and priority are app-authored descriptions of typed Windows values.
// File resource names, company text, paths, command lines and signer names never
// pass through this localization boundary.
fn architecture_text(value: &Option<String>) -> String {
    let Some(value) = value.as_deref().filter(|value| !value.is_empty()) else {
        return "—".into();
    };
    match value {
        "x86（32 位）" => trs("x86（32 位）"),
        "x64（64 位）" => trs("x64（64 位）"),
        "ARM Thumb-2（32 位）" => trs("ARM Thumb-2（32 位）"),
        _ => {
            if let Some(raw_machine) = value.strip_prefix("机器架构 0x") {
                tf(
                    "机器架构 0x{value:04X}",
                    &[("value", raw_machine.to_owned())],
                )
            } else {
                value.to_owned()
            }
        }
    }
}

fn priority_text(details: &ProcessDetails) -> String {
    use windows::Win32::System::Threading::{
        ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS,
        IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, REALTIME_PRIORITY_CLASS,
    };
    match details.priority_class {
        Some(value) if value == IDLE_PRIORITY_CLASS.0 => trs("低"),
        Some(value) if value == BELOW_NORMAL_PRIORITY_CLASS.0 => trs("低于正常"),
        Some(value) if value == NORMAL_PRIORITY_CLASS.0 => trs("正常"),
        Some(value) if value == ABOVE_NORMAL_PRIORITY_CLASS.0 => trs("高于正常"),
        Some(value) if value == HIGH_PRIORITY_CLASS.0 => trs("高"),
        Some(value) if value == REALTIME_PRIORITY_CLASS.0 => trs("实时"),
        Some(value) => tf("优先级 0x{value:08X}", &[("value", format!("{value:08X}"))]),
        None => optional_text(&details.priority),
    }
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
        self.file_signature = None;
        self.file_signature_loading = false;
        self.file_signature_error = None;
        self.file_signature_generation = self.file_signature_generation.wrapping_add(1);
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
                        Some(msg("所选进程已退出或身份已改变，详情已过期"));
                } else {
                    match result {
                        Ok(details) => {
                            this.process_details = Some(details);
                            this.process_details_error = None;
                        }
                        Err(error) => {
                            this.process_details = None;
                            this.process_details_error = Some(mf(
                                "进程详情读取失败：{error:#}",
                                &[("error", format!("{error:#}"))],
                            ));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn verify_selected_signature(&mut self, cx: &mut Context<Self>) {
        if self.file_signature_loading {
            return;
        }
        let Some(identity) = self.process_details_identity else {
            return;
        };
        self.file_signature_loading = true;
        self.file_signature = None;
        self.file_signature_error = None;
        self.file_signature_generation = self.file_signature_generation.wrapping_add(1);
        let generation = self.file_signature_generation;
        cx.spawn(async move |view, cx| {
            let result = smol::unblock(move || -> anyhow::Result<_> {
                use anyhow::{Context as _, ensure};
                let before = ProcessDetails::read(identity.0, identity.1)?;
                let path = before
                    .executable_path
                    .as_deref()
                    .context(trs("此进程的可执行文件路径不可用"))?;
                let signature = system_monitor_x::file_signature::verify_file_signature(
                    std::path::Path::new(path),
                )?;
                let after = ProcessDetails::read(identity.0, identity.1)?;
                let after_path = after
                    .executable_path
                    .as_deref()
                    .context(trs("验签后进程文件路径不可用"))?;
                ensure!(
                    std::path::Path::new(after_path).canonicalize()? == signature.file_path,
                    "{}",
                    trs("验签期间进程文件路径已变化")
                );
                Ok(Arc::new(signature))
            })
            .await;
            let _ = view.update(cx, |this, cx| {
                if this.process_details_identity != Some(identity)
                    || this.file_signature_generation != generation
                {
                    return;
                }
                this.file_signature_loading = false;
                let still_alive = this.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.processes.iter().any(|p| {
                        p.pid == identity.0 && p.start_time_windows_ticks == Some(identity.1)
                    })
                });
                if !still_alive {
                    this.file_signature = None;
                    this.file_signature_error = Some(msg("所选进程已退出或身份已变化，请重新选择"));
                } else {
                    match result {
                        Ok(signature) => this.file_signature = Some(signature),
                        Err(error) => {
                            this.file_signature_error = Some(mf(
                                "文件签名验证不可用：{error:#}",
                                &[("error", format!("{error:#}"))],
                            ))
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
                .child(tr("无法取得精确进程身份，详细信息暂不可用"))
                .into_any_element();
        };
        let details = self
            .process_details
            .as_deref()
            .filter(|details| details.pid == identity.0 && details.creation_ticks == identity.1);
        let summary = if self.process_details_loading {
            trs("正在后台读取…")
        } else if self.process_details_error.is_some() {
            trs("部分详情不可用")
        } else if let Some(details) = details {
            format!(
                "{} · {}",
                architecture_text(&details.architecture),
                optional_text(&details.file_version)
            )
        } else {
            trs("按所选进程读取")
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
                                tr("收起详情")
                            } else {
                                tr("进程详情")
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
                            Button::new("verify-file-signature")
                                .small()
                                .ghost()
                                .label(if self.file_signature_loading {
                                    tr("正在验证签名…")
                                } else {
                                    tr("验证文件签名")
                                })
                                .disabled(
                                    self.file_signature_loading
                                        || details
                                            .and_then(|d| d.executable_path.as_ref())
                                            .is_none(),
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.verify_selected_signature(cx)
                                    }),
                                ),
                        )
                    })
                    .when(expanded, |element| {
                        element.child(
                            Button::new("refresh-process-details")
                                .small()
                                .ghost()
                                .label(tr("刷新详情"))
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
                            .child(error.render()),
                    );
                }
                if let Some(error) = self.file_signature_error.as_ref() {
                    body = body.child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(error.render()),
                    );
                }
                if let Some(signature) = self.file_signature.as_deref() {
                    use system_monitor_x::file_signature::{SignatureKind, SignatureStatus};
                    let status = match signature.status {
                        SignatureStatus::VerifiedOffline => trs("本机离线信任验证通过"),
                        SignatureStatus::Unsigned => trs("未发现嵌入式或本机目录签名"),
                        SignatureStatus::Invalid => trs("签名无效或文件摘要不匹配"),
                        SignatureStatus::Untrusted => trs("签名证书链未被信任"),
                        SignatureStatus::Unavailable => trs("签名验证不可用或不完整"),
                    };
                    let kind = match signature.signature_kind {
                        SignatureKind::Embedded => trs("嵌入式签名"),
                        SignatureKind::Catalog => trs("本机目录签名"),
                        SignatureKind::None => trs("无已发现签名"),
                        SignatureKind::Unknown => trs("未知"),
                    };
                    body = body
                        .child(Self::detail_row(
                            tr("磁盘文件签名（本次验证）"),
                            format!("{status} · {kind}"),
                            cx,
                        ))
                        .child(Self::detail_row(
                            tr("已验证签名发布者"),
                            optional_text(&signature.signer_name),
                            cx,
                        ))
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(tf(
                                    "原始诊断：{issue}",
                                    &[("issue", signature.reason.clone())],
                                ))
                                .child(
                                    div().text_xs().child(tr(
                                        "仅使用本机证书信任与缓存；未联网查询证书吊销状态",
                                    )),
                                ),
                        )
                        .children(signature.issues.iter().map(|issue| {
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(tf("原始诊断：{issue}", &[("issue", issue.clone())]))
                        }));
                    if let Some(timestamp) = &signature.timestamp {
                        body = body.child(Self::detail_row(
                            tr("可信时间戳机构"),
                            optional_text(&timestamp.authority_name),
                            cx,
                        ));
                    }
                }
                if let Some(details) = details {
                    let rows = [
                        ("命令行", optional_text(&details.command_line)),
                        (
                            "进程 / 系统架构",
                            format!(
                                "{} / {}",
                                architecture_text(&details.architecture),
                                architecture_text(&details.native_architecture)
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
                                priority_text(details),
                                details
                                    .efficiency_mode
                                    .map(|enabled| trs(if enabled {
                                        "已启用"
                                    } else {
                                        "未启用"
                                    }))
                                    .unwrap_or_else(|| "—".into())
                            ),
                        ),
                        ("当前目录", optional_text(&details.current_directory)),
                    ];
                    body = body
                        .children(
                            rows.into_iter()
                                .map(|(label, value)| Self::detail_row(tr(label), value, cx)),
                        )
                        .child(
                            div().text_color(cx.theme().muted_foreground).child(
                                details
                                    .file_metadata_source
                                    .as_deref()
                                    .map(trs)
                                    .unwrap_or_else(|| {
                                        trs("版本资源未提供；公司文字不能作为已验证的签名发布者")
                                    }),
                            ),
                        )
                        .children(details.issues.iter().map(|issue| {
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(tf("原始诊断：{issue}", &[("issue", issue.clone())]))
                        }))
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr("详情在选择进程或点击刷新时读取")),
                        );
                } else if self.process_details_loading {
                    body = body.child(tr("正在后台读取所选进程的详细信息…"));
                }
                element.child(body)
            })
            .into_any_element()
    }
}
