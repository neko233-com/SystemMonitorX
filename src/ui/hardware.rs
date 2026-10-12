use super::localization::{tf_in, trs_in};
use super::*;
use system_monitor_x::{cpu_topology::CacheKind, hardware::HardwareInventory, i18n::Language};

#[derive(Clone)]
pub(super) struct HardwareSection {
    title: String,
    rows: Vec<(String, String)>,
}
fn text(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "—".into())
}
fn capacity(value: Option<u64>) -> String {
    value.map(bytes).unwrap_or_else(|| "—".into())
}
fn number(value: Option<u32>, suffix: &str) -> String {
    value
        .map(|n| format!("{n}{suffix}"))
        .unwrap_or_else(|| "—".into())
}
fn affinity_text(
    language: Language,
    values: &[system_monitor_x::cpu_topology::ProcessorAffinity],
) -> String {
    values
        .iter()
        .map(|affinity| {
            let processors = affinity
                .logical_processor_numbers()
                .into_iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(",");
            tf_in(
                language,
                "组 {}：CPU {processors}",
                &[
                    ("arg0", format!("{}", affinity.group)),
                    ("processors", processors.to_string()),
                ],
            )
        })
        .collect::<Vec<_>>()
        .join("；")
}
fn boolean(language: Language, value: Option<bool>, yes: &str, no: &str) -> String {
    value
        .map(|value| trs_in(language, if value { yes } else { no }))
        .unwrap_or_else(|| "—".into())
}
fn section(title: impl Into<String>, rows: Vec<(String, String)>) -> HardwareSection {
    HardwareSection {
        title: title.into(),
        rows,
    }
}

fn duration_in(language: Language, secs: u64) -> String {
    tf_in(
        language,
        "{}天 {:02}:{:02}:{:02}",
        &[
            ("arg0", format!("{}", secs / 86400)),
            ("arg1", format!("{:02}", secs / 3600 % 24)),
            ("arg2", format!("{:02}", secs / 60 % 60)),
            ("arg3", format!("{:02}", secs % 60)),
        ],
    )
}
fn cache_kind_text(language: Language, kind: &CacheKind) -> String {
    match kind {
        CacheKind::Unified => trs_in(language, "统一缓存"),
        CacheKind::Instruction => trs_in(language, "指令"),
        CacheKind::Data => trs_in(language, "数据"),
        CacheKind::Trace => trs_in(language, "追踪"),
        CacheKind::Unknown(value) => tf_in(
            language,
            "未知类型 {value}",
            &[("value", value.to_string())],
        ),
    }
}

fn presentation_request_is_current(
    requested_generation: u64,
    requested_language: Language,
    current_generation: u64,
    current_language: Language,
) -> bool {
    requested_generation == current_generation && requested_language == current_language
}

/// Format static firmware fields on the inventory worker too. Live UI frames
/// reuse these strings instead of repeating WMI queries or device formatting.
fn build_sections(h: &HardwareInventory, language: Language) -> Vec<HardwareSection> {
    let mut sections = Vec::new();
    if let Some(system) = &h.system {
        sections.push(section(
            trs_in(language, "整机"),
            vec![
                (
                    trs_in(language, "厂商 / 型号"),
                    format!("{} · {}", text(&system.manufacturer), text(&system.model)),
                ),
                (trs_in(language, "系统架构"), text(&system.system_type)),
            ],
        ));
    }
    if let Some(topology) = &h.cpu_topology {
        let mut rows = vec![
            (
                trs_in(language, "拓扑读取状态"),
                trs_in(
                    language,
                    if topology.complete {
                        "完整"
                    } else {
                        "部分可用"
                    },
                ),
            ),
            (
                trs_in(language, "物理插槽 / 核心 / 逻辑处理器"),
                format!(
                    "{} / {} / {}",
                    topology.packages.len(),
                    topology.cores.len(),
                    number(topology.logical_processor_count, "")
                ),
            ),
            (
                trs_in(language, "NUMA 节点 / 活跃处理器组"),
                format!(
                    "{} / {}",
                    topology.numa_nodes.len(),
                    number(topology.active_group_count.map(u32::from), "")
                ),
            ),
            (
                trs_in(language, "启用 SMT 的核心"),
                topology
                    .cores
                    .iter()
                    .filter(|core| core.smt)
                    .count()
                    .to_string(),
            ),
        ];
        let mut classes = std::collections::BTreeMap::<u8, usize>::new();
        for core in &topology.cores {
            *classes.entry(core.efficiency_class).or_default() += 1;
        }
        rows.push((
            trs_in(language, "Windows 性能 / 能效类（原值）"),
            classes
                .into_iter()
                .map(|(class, count)| {
                    tf_in(
                        language,
                        "类 {class}：{count} 核心",
                        &[("class", format!("{class}")), ("count", format!("{count}"))],
                    )
                })
                .collect::<Vec<_>>()
                .join("；"),
        ));
        for group in &topology.groups {
            rows.push((
                tf_in(
                    language,
                    "处理器组 {}",
                    &[("arg0", format!("{}", group.number))],
                ),
                tf_in(
                    language,
                    "{} 活跃 / {} 上限 · 掩码 0x{:X}",
                    &[
                        ("arg0", format!("{}", group.active_processor_count)),
                        ("arg1", format!("{}", group.maximum_processor_count)),
                        ("arg2", format!("{:X}", group.active_processor_mask)),
                    ],
                ),
            ));
        }
        for node in &topology.numa_nodes {
            rows.push((
                format!("NUMA {}", node.node_number),
                affinity_text(language, &node.affinities),
            ));
        }
        for package in &topology.packages {
            rows.push((
                tf_in(
                    language,
                    "物理插槽 {}",
                    &[("arg0", format!("{}", package.index))],
                ),
                affinity_text(language, &package.affinities),
            ));
        }
        sections.push(section(trs_in(language, "CPU 原生拓扑"), rows));
        sections.push(section(
            trs_in(language, "CPU 核心与逻辑处理器"),
            topology
                .cores
                .iter()
                .map(|core| {
                    (
                        tf_in(
                            language,
                            "核心 {} · 插槽 {}",
                            &[
                                ("arg0", format!("{}", core.index)),
                                ("arg1", number(core.package_index, "").to_string()),
                            ],
                        ),
                        tf_in(
                            language,
                            "{} · 类 {} · {}",
                            &[
                                (
                                    "arg0",
                                    trs_in(language, if core.smt { "SMT" } else { "单线程核心" })
                                        .to_string(),
                                ),
                                ("arg1", format!("{}", core.efficiency_class)),
                                (
                                    "arg2",
                                    affinity_text(language, &core.affinities).to_string(),
                                ),
                            ],
                        ),
                    )
                })
                .collect(),
        ));
        sections.push(section(
            trs_in(language, "CPU 原生缓存"),
            topology
                .caches
                .iter()
                .map(|cache| {
                    (
                        format!(
                            "L{} {} · #{}",
                            cache.level,
                            cache_kind_text(language, &cache.kind),
                            cache.index
                        ),
                        tf_in(
                            language,
                            "{} · 缓存行 {} B · {} · {}",
                            &[
                                ("arg0", bytes(cache.size_bytes).to_string()),
                                ("arg1", format!("{}", cache.line_size_bytes)),
                                (
                                    "arg2",
                                    (match cache.associativity {
                                        0 => trs_in(language, "相联度未提供"),
                                        255 => trs_in(language, "全相联"),
                                        value => tf_in(
                                            language,
                                            "{value} 路相联",
                                            &[("value", value.to_string())],
                                        ),
                                    })
                                    .to_string(),
                                ),
                                (
                                    "arg3",
                                    affinity_text(language, &cache.affinities).to_string(),
                                ),
                            ],
                        ),
                    )
                })
                .collect(),
        ));
        if !topology.dies.is_empty() {
            sections.push(section(
                trs_in(language, "CPU 晶粒"),
                topology
                    .dies
                    .iter()
                    .map(|die| {
                        (
                            tf_in(language, "晶粒 {}", &[("arg0", format!("{}", die.index))]),
                            affinity_text(language, &die.affinities),
                        )
                    })
                    .collect(),
            ));
        }
        if !topology.modules.is_empty() {
            sections.push(section(
                trs_in(language, "CPU 模块"),
                topology
                    .modules
                    .iter()
                    .map(|module| {
                        (
                            tf_in(
                                language,
                                "模块 {}",
                                &[("arg0", format!("{}", module.index))],
                            ),
                            affinity_text(language, &module.affinities),
                        )
                    })
                    .collect(),
            ));
        }
    }
    if let Some(power) = &h.power_status {
        sections.push(section(
            trs_in(language, "电源与电池"),
            vec![
                (
                    trs_in(language, "外接电源"),
                    boolean(language, power.ac_online, "已连接", "未连接"),
                ),
                (
                    trs_in(language, "系统电池"),
                    boolean(language, power.battery_present, "已检测到", "无系统电池"),
                ),
                (
                    trs_in(language, "电量"),
                    power
                        .battery_percent
                        .map(|value| format!("{value}%"))
                        .unwrap_or_else(|| "—".into()),
                ),
                (
                    trs_in(language, "充电 / 节电模式"),
                    format!(
                        "{} / {}",
                        boolean(language, power.charging, "充电中", "未充电"),
                        boolean(language, power.battery_saver, "已启用", "未启用")
                    ),
                ),
                (
                    trs_in(language, "预计剩余续航"),
                    power
                        .remaining_seconds
                        .map(|value| duration_in(language, u64::from(value)))
                        .unwrap_or_else(|| "—".into()),
                ),
                (
                    trs_in(language, "状态刷新"),
                    trs_in(language, "随设备信息刷新；可手动重新读取"),
                ),
            ],
        ));
    }
    for battery in &h.batteries {
        sections.push(section(
            tf_in(
                language,
                "电池 · {}",
                &[("arg0", battery.device_id.to_string())],
            ),
            vec![
                (trs_in(language, "名称 / 型号"), text(&battery.name)),
                (
                    trs_in(language, "化学类型 / 状态"),
                    format!("{} / {}", text(&battery.chemistry), text(&battery.status)),
                ),
                (
                    trs_in(language, "电量"),
                    battery
                        .estimated_charge_percent
                        .map(|value| format!("{value}%"))
                        .unwrap_or_else(|| "—".into()),
                ),
                (
                    trs_in(language, "设计容量 / 满充容量"),
                    format!(
                        "{} / {}",
                        battery
                            .design_capacity_mwh
                            .map(|value| format!("{value} mWh"))
                            .unwrap_or_else(|| "—".into()),
                        battery
                            .full_charge_capacity_mwh
                            .map(|value| format!("{value} mWh"))
                            .unwrap_or_else(|| "—".into())
                    ),
                ),
                (
                    trs_in(language, "设计电压"),
                    battery
                        .design_voltage_mv
                        .map(|value| format!("{value} mV"))
                        .unwrap_or_else(|| "—".into()),
                ),
                (trs_in(language, "设备标识"), text(&battery.pnp_device_id)),
            ],
        ));
    }
    for board in &h.motherboards {
        sections.push(section(
            trs_in(language, "主板"),
            vec![
                (
                    trs_in(language, "厂商 / 产品"),
                    format!("{} · {}", text(&board.manufacturer), text(&board.product)),
                ),
                (trs_in(language, "版本"), text(&board.version)),
                (trs_in(language, "序列号"), text(&board.serial_number)),
            ],
        ));
    }
    for bios in &h.bios {
        sections.push(section(
            trs_in(language, "BIOS / 固件"),
            vec![
                (
                    trs_in(language, "厂商 / 版本"),
                    format!("{} · {}", text(&bios.manufacturer), text(&bios.version)),
                ),
                (trs_in(language, "发布日期"), text(&bios.release_date)),
                ("SMBIOS".into(), text(&bios.smbios_version)),
            ],
        ));
    }
    for cpu in &h.processors {
        sections.push(section(
            tf_in(
                language,
                "处理器 · {}",
                &[("arg0", cpu.device_id.to_string())],
            ),
            vec![
                (trs_in(language, "型号"), text(&cpu.name)),
                (
                    trs_in(language, "插槽 / 厂商"),
                    format!("{} · {}", text(&cpu.socket), text(&cpu.manufacturer)),
                ),
                (
                    trs_in(language, "核心 / 线程"),
                    format!(
                        "{} / {}",
                        number(cpu.physical_cores, ""),
                        number(cpu.logical_processors, "")
                    ),
                ),
                (
                    trs_in(language, "标称最高频率"),
                    number(cpu.max_clock_mhz, " MHz"),
                ),
                (
                    trs_in(language, "L2 / L3 缓存"),
                    format!(
                        "{} / {}",
                        capacity(cpu.l2_cache_bytes),
                        capacity(cpu.l3_cache_bytes)
                    ),
                ),
                (
                    trs_in(language, "固件虚拟化"),
                    cpu.virtualization_firmware_enabled
                        .map(|v| trs_in(language, if v { "已启用" } else { "未启用" }))
                        .unwrap_or_else(|| trs_in(language, "不可用")),
                ),
            ],
        ));
    }
    if !h.cpu_caches.is_empty() {
        sections.push(section(
            trs_in(language, "处理器缓存"),
            h.cpu_caches
                .iter()
                .map(|cache| {
                    (
                        format!(
                            "{} · {}",
                            cache.device_id,
                            cache
                                .level
                                .map(|value| tf_in(
                                    language,
                                    "{value} 级",
                                    &[("value", value.to_string())]
                                ))
                                .unwrap_or_else(|| "—".into())
                        ),
                        format!(
                            "{} · {}",
                            capacity(cache.size_bytes),
                            text(&cache.cache_type)
                        ),
                    )
                })
                .collect(),
        ));
    }
    sections.push(section(
        trs_in(language, "内存插槽"),
        vec![
            (
                trs_in(language, "已用 / 总槽位"),
                format!(
                    "{} / {}",
                    h.memory_modules.len(),
                    number(h.memory_slots_total, "")
                ),
            ),
            (
                trs_in(language, "固件最大容量"),
                capacity(h.memory_max_capacity_bytes),
            ),
        ],
    ));
    for (index, memory) in h.memory_modules.iter().enumerate() {
        sections.push(section(
            tf_in(
                language,
                "内存模块 {} · {}",
                &[
                    ("arg0", format!("{}", index + 1)),
                    ("arg1", text(&memory.device_locator).to_string()),
                ],
            ),
            vec![
                (
                    trs_in(language, "容量 / 类型"),
                    format!(
                        "{} · {} · {}",
                        capacity(memory.capacity_bytes),
                        text(&memory.memory_type),
                        text(&memory.form_factor)
                    ),
                ),
                (
                    trs_in(language, "当前 / 标称速率"),
                    format!(
                        "{} / {}",
                        number(memory.configured_speed_mts, " MT/s"),
                        number(memory.rated_speed_mts, " MT/s")
                    ),
                ),
                (
                    trs_in(language, "厂商 / 料号"),
                    format!(
                        "{} · {}",
                        text(&memory.manufacturer),
                        text(&memory.part_number)
                    ),
                ),
                (trs_in(language, "序列号"), text(&memory.serial_number)),
                (
                    trs_in(language, "数据 / 总线宽度"),
                    format!(
                        "{} / {}",
                        number(memory.data_width_bits, " bit"),
                        number(memory.total_width_bits, " bit")
                    ),
                ),
            ],
        ));
    }
    for disk in &h.physical_disks {
        sections.push(section(
            tf_in(
                language,
                "物理存储 · {}",
                &[("arg0", disk.device_id.to_string())],
            ),
            vec![
                (
                    trs_in(language, "型号 / 容量"),
                    format!("{} · {}", text(&disk.model), capacity(disk.capacity_bytes)),
                ),
                (
                    trs_in(language, "接口 / 介质"),
                    format!("{} · {}", text(&disk.bus_type), text(&disk.media_type)),
                ),
                (
                    trs_in(language, "固件 / 序列号"),
                    format!(
                        "{} · {}",
                        text(&disk.firmware_version),
                        text(&disk.serial_number)
                    ),
                ),
                (
                    trs_in(language, "逻辑 / 物理扇区"),
                    format!(
                        "{} / {}",
                        capacity(disk.logical_sector_bytes),
                        capacity(disk.physical_sector_bytes)
                    ),
                ),
                (
                    trs_in(language, "健康状态 / 来源"),
                    format!("{} · {}", text(&disk.health_status), disk.source),
                ),
            ],
        ));
    }
    for nic in &h.network_adapters {
        let link = |value: Option<u64>| {
            value
                .map(|n| format!("{:.2} Gbps", n as f64 / 1_000_000_000.))
                .unwrap_or_else(|| "—".into())
        };
        sections.push(section(
            tf_in(language, "网卡 · {}", &[("arg0", nic.name.to_string())]),
            vec![
                (
                    trs_in(language, "硬件 / 状态"),
                    format!(
                        "{} · {}",
                        text(&nic.description),
                        text(&nic.connection_state)
                    ),
                ),
                (
                    trs_in(language, "MAC / 接口索引"),
                    format!(
                        "{} · {}",
                        text(&nic.mac_address),
                        number(nic.interface_index, "")
                    ),
                ),
                (
                    trs_in(language, "接收 / 发送链路速率"),
                    format!(
                        "{} / {}",
                        link(nic.receive_link_bits_per_sec),
                        link(nic.transmit_link_bits_per_sec)
                    ),
                ),
                (
                    trs_in(language, "驱动 / 提供商"),
                    format!(
                        "{} · {}",
                        text(&nic.driver_version),
                        text(&nic.driver_provider)
                    ),
                ),
                (trs_in(language, "驱动日期"), text(&nic.driver_date)),
                (trs_in(language, "硬件标识"), text(&nic.pnp_device_id)),
            ],
        ));
    }
    for graphics in &h.graphics_adapters {
        sections.push(section(
            format!(
                "{} · {}",
                trs_in(
                    language,
                    if graphics.is_pci_device {
                        "显卡驱动"
                    } else {
                        "虚拟显示驱动"
                    }
                ),
                graphics.name
            ),
            vec![
                (
                    trs_in(language, "驱动 / 日期"),
                    format!(
                        "{} · {}",
                        text(&graphics.driver_version),
                        text(&graphics.driver_date)
                    ),
                ),
                (
                    trs_in(language, "当前显示模式"),
                    format!(
                        "{} × {} · {}",
                        number(graphics.active_width_pixels, ""),
                        number(graphics.active_height_pixels, ""),
                        number(graphics.active_refresh_hz, " Hz")
                    ),
                ),
                (trs_in(language, "硬件标识"), text(&graphics.pnp_device_id)),
            ],
        ));
    }
    for monitor in &h.monitors {
        sections.push(section(
            tf_in(
                language,
                "显示器 · {}",
                &[("arg0", text(&monitor.friendly_name).to_string())],
            ),
            vec![
                (
                    trs_in(language, "厂商 / 序列号"),
                    format!(
                        "{} · {}",
                        text(&monitor.manufacturer),
                        text(&monitor.serial_number)
                    ),
                ),
                (
                    trs_in(language, "生产年 / 周"),
                    format!(
                        "{} / {}",
                        number(monitor.manufacture_year, ""),
                        number(monitor.manufacture_week, "")
                    ),
                ),
                (trs_in(language, "设备标识"), monitor.instance_name.clone()),
            ],
        ));
    }
    sections
}
impl SystemMonitorApp {
    pub(super) fn refresh_hardware(&mut self, cx: &mut Context<Self>) {
        if self.hardware_loading {
            return;
        }
        self.hardware_loading = true;
        self.hardware_display_generation = self.hardware_display_generation.wrapping_add(1);
        let generation = self.hardware_display_generation;
        let language = self.resolved_language;
        cx.spawn(async move |view, cx| {
            let result = smol::unblock(move || {
                HardwareInventory::collect().map(|inventory| {
                    let sections = build_sections(&inventory, language);
                    (Arc::new(inventory), sections)
                })
            })
            .await;
            let _ = view.update(cx, |this, cx| {
                this.hardware_loading = false;
                match result {
                    Ok((inventory, sections)) => {
                        // A language switch does not discard real collection.
                        // Only the obsolete presentation is rejected.
                        this.hardware = Some(inventory);
                        this.hardware_error = None;
                        if presentation_request_is_current(
                            generation,
                            language,
                            this.hardware_display_generation,
                            this.resolved_language,
                        ) {
                            this.hardware_sections = sections;
                        } else {
                            this.rebuild_hardware_presentation(cx);
                        }
                    }
                    Err(error) => {
                        // Diagnostics retain their source language; translate
                        // the surrounding label only when rendering.
                        this.hardware_error = Some(format!("{error:#}"));
                        if this.hardware.is_some() {
                            this.rebuild_hardware_presentation(cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Reformat the cached inventory without issuing WMI/Win32 queries.
    /// The request generation also rejects A -> B -> A completion races.
    pub(super) fn rebuild_hardware_presentation(&mut self, cx: &mut Context<Self>) {
        self.hardware_display_generation = self.hardware_display_generation.wrapping_add(1);
        let generation = self.hardware_display_generation;
        let language = self.resolved_language;
        self.hardware_sections.clear();
        let Some(inventory) = self.hardware.clone() else {
            cx.notify();
            return;
        };
        let expected_inventory = inventory.clone();
        cx.spawn(async move |view, cx| {
            let sections = smol::unblock(move || build_sections(&inventory, language)).await;
            let _ = view.update(cx, |this, cx| {
                if presentation_request_is_current(
                    generation,
                    language,
                    this.hardware_display_generation,
                    this.resolved_language,
                ) && this
                    .hardware
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &expected_inventory))
                {
                    this.hardware_sections = sections;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn hardware_details(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_5()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr("硬件详细信息")),
                    )
                    .child(
                        Button::new("refresh-hardware")
                            .small()
                            .label(tr(if self.hardware_loading {
                                "读取中…"
                            } else {
                                "刷新硬件"
                            }))
                            .disabled(self.hardware_loading)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_hardware(cx))),
                    ),
            )
            .when(self.hardware.is_none() && self.hardware_loading, |e| {
                e.child(tr("正在后台读取固件与驱动信息…"))
            })
            .when(
                self.hardware.is_some() && self.hardware_sections.is_empty(),
                |e| e.child(tr("正在更新硬件显示…")),
            )
            .when_some(self.hardware_error.clone(), |e, error| {
                e.child(tf("硬件读取失败：{error:#}", &[("error", error)]))
            })
            .children(self.hardware_sections.iter().map(|section| {
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(section.title.clone()),
                    )
                    .children(
                        section.rows.iter().map(|(label, value)| {
                            Self::detail_row(label.clone(), value.clone(), cx)
                        }),
                    )
            }))
            .when_some(self.hardware.as_ref(), |e, inventory| {
                e.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tf(
                            "来源：Windows WMI / 固件 · 此次读取 {} ms · 静态信息按需刷新",
                            &[("arg0", format!("{}", inventory.collection_duration_ms))],
                        )),
                )
                .children(inventory.issues.iter().map(|issue| {
                    div()
                        .text_xs()
                        .child(tf("原始诊断：{issue}", &[("issue", issue.clone())]))
                }))
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{affinity_text, build_sections, cache_kind_text, presentation_request_is_current};
    use std::sync::Arc;
    use system_monitor_x::{
        cpu_topology::{CacheKind, ProcessorAffinity},
        hardware::{HardwareInventory, ProcessorInfo, SystemHardware, SystemPowerStatus},
        i18n::{Language, text_source_in},
    };

    #[test]
    fn hardware_models_use_captured_language_and_preserve_real_names() {
        let raw_manufacturer = "整机 %{issue}";
        let raw_model = "型号 / firmware-β";
        let inventory = Arc::new(HardwareInventory {
            system: Some(SystemHardware {
                manufacturer: Some(raw_manufacturer.into()),
                model: Some(raw_model.into()),
                system_type: Some("原始 Windows 架构".into()),
                ..Default::default()
            }),
            processors: vec![ProcessorInfo {
                device_id: "CPU-raw".into(),
                name: Some("核心与真实品牌 %{value}".into()),
                virtualization_firmware_enabled: None,
                ..Default::default()
            }],
            power_status: Some(SystemPowerStatus {
                ac_online: Some(true),
                battery_present: None,
                ..Default::default()
            }),
            ..Default::default()
        });
        for language in Language::SUPPORTED {
            let sections = build_sections(&inventory, language);
            assert_eq!(sections[0].title, text_source_in(language, "整机"));
            if language != Language::ZhCn {
                assert_ne!(sections[0].title, "整机");
            }
            assert_eq!(
                sections[0].rows[0].1,
                format!("{raw_manufacturer} · {raw_model}"),
            );
            assert_eq!(sections[0].rows[1].1, "原始 Windows 架构");
            let power = sections
                .iter()
                .find(|section| section.title == text_source_in(language, "电源与电池"))
                .unwrap();
            assert_eq!(power.rows[0].1, text_source_in(language, "已连接"));
            assert_eq!(power.rows[1].1, "—"); // Unknown battery is not "absent".
            let processor = sections
                .iter()
                .find(|section| {
                    section
                        .rows
                        .iter()
                        .any(|row| row.1 == "核心与真实品牌 %{value}")
                })
                .unwrap();
            assert_eq!(
                processor.rows.last().unwrap().1,
                text_source_in(language, "不可用"),
            );
        }
    }

    #[test]
    fn topology_values_and_unknown_type_keep_exact_numeric_arguments() {
        let affinities = [ProcessorAffinity {
            group: 3,
            mask: 0b101,
        }];
        for language in Language::SUPPORTED {
            let affinity = affinity_text(language, &affinities);
            assert!(affinity.contains('3'));
            assert!(affinity.contains("0,2"));
            let unknown = cache_kind_text(language, &CacheKind::Unknown(219));
            assert!(unknown.contains("219"));
            assert!(!unknown.contains("{value}"));
        }
    }

    #[test]
    fn obsolete_hardware_completion_is_rejected_after_language_aba() {
        let first = (1, Language::EnUs);
        let second = (2, Language::JaJp);
        let current = (3, Language::EnUs);
        assert!(!presentation_request_is_current(
            first.0, first.1, current.0, current.1,
        ));
        assert!(!presentation_request_is_current(
            second.0, second.1, current.0, current.1,
        ));
        assert!(presentation_request_is_current(
            current.0, current.1, current.0, current.1,
        ));
        assert!(!presentation_request_is_current(
            current.0,
            Language::KoKr,
            current.0,
            current.1,
        ));
    }
}
