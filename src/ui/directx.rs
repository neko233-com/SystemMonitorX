use super::*;
use system_monitor_x::directx::DirectXInventory;

impl SystemMonitorApp {
    pub(super) fn refresh_directx(&mut self, cx: &mut Context<Self>) {
        if self.directx_loading {
            return;
        }
        self.directx_loading = true;
        self.directx_error = None;
        cx.spawn(async move |view, cx| {
            let result = smol::unblock(DirectXInventory::collect).await;
            let _ = view.update(cx, |this, cx| {
                this.directx_loading = false;
                match result {
                    Ok(inventory) => this.directx_inventory = Some(Arc::new(inventory)),
                    Err(error) => this.directx_error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn directx_details(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body=v_flex().gap_3().child(
            h_flex().justify_between().child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(tr("DirectX 12 与图形功能")))
                .child(Button::new("refresh-directx").small().ghost().label(tr("重新检测")).disabled(self.directx_loading).on_click(cx.listener(|this,_,_,cx|this.refresh_directx(cx))))
        ).child(div().text_xs().text_color(cx.theme().muted_foreground).child(tr("按 DXGI LUID 查询每张显卡的 Windows 运行时和驱动能力。GPU 负载采集覆盖 DirectX 12 等 WDDM 工作负载。")));
        if self.directx_loading {
            body = body.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("正在后台查询图形设备能力…")),
            );
        }
        if let Some(error) = &self.directx_error {
            body = body.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(tf("原始诊断：{issue}", &[("issue", error.clone())])),
            );
        }
        if let Some(inventory) = &self.directx_inventory {
            for adapter in inventory
                .adapters
                .iter()
                .filter(|a| !a.is_software && a.is_virtual != Some(true))
            {
                let unavailable = || "—".to_owned();
                body = body.child(
                    v_flex()
                        .gap_1()
                        .p_4()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(adapter.name.clone()),
                        )
                        .children(
                            [
                                (
                                    "DirectX 12",
                                    adapter
                                        .d3d12_supported
                                        .map(|v| trs(if v { "支持" } else { "不支持" }))
                                        .unwrap_or_else(unavailable),
                                ),
                                (
                                    "功能级别 / Shader Model",
                                    format!(
                                        "{} / {}",
                                        adapter.feature_level.as_deref().unwrap_or("—"),
                                        adapter.shader_model.as_deref().unwrap_or("—")
                                    ),
                                ),
                                (
                                    "光线追踪",
                                    adapter.raytracing_tier.clone().unwrap_or_else(unavailable),
                                ),
                                (
                                    "Mesh Shader",
                                    adapter.mesh_shader_tier.clone().unwrap_or_else(unavailable),
                                ),
                                (
                                    "Sampler Feedback",
                                    adapter
                                        .sampler_feedback_tier
                                        .clone()
                                        .unwrap_or_else(unavailable),
                                ),
                                (
                                    "Enhanced Barriers",
                                    adapter
                                        .enhanced_barriers
                                        .map(|v| trs(if v { "支持" } else { "不支持" }))
                                        .unwrap_or_else(unavailable),
                                ),
                                (
                                    "GPU 节点 / LUID",
                                    format!(
                                        "{} / {:016x}",
                                        adapter
                                            .node_count
                                            .map(|n| n.to_string())
                                            .unwrap_or_else(unavailable),
                                        adapter.luid
                                    ),
                                ),
                            ]
                            .into_iter()
                            .map(|(label, value)| {
                                Self::detail_row(
                                    if matches!(
                                        label,
                                        "功能级别 / Shader Model" | "光线追踪" | "GPU 节点 / LUID"
                                    ) {
                                        tr(label)
                                    } else {
                                        label.into()
                                    },
                                    value,
                                    cx,
                                )
                            }),
                        )
                        .children(adapter.issues.iter().map(|note| {
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(tf("原始诊断：{issue}", &[("issue", note.clone())]))
                        })),
                );
            }
            let aliases: Vec<_> = inventory
                .adapters
                .iter()
                .filter(|a| a.is_software || a.is_virtual == Some(true))
                .map(|a| format!("{}（{}）", a.name, a.classification))
                .collect();
            if !aliases.is_empty() {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tf(
                            "软件 / 虚拟显示适配器：{}",
                            &[("arg0", aliases.join(", ").to_string())],
                        )),
                );
            }
            body = body.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tf(
                        "来源：DXGI + D3D12 CheckFeatureSupport · 耗时 {} ms",
                        &[("arg0", format!("{}", inventory.collection_duration_ms))],
                    )),
            );
            body = body.children(inventory.issues.iter().map(|note| {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tf("原始诊断：{issue}", &[("issue", note.clone())]))
            }));
        }
        body.into_any_element()
    }
}
