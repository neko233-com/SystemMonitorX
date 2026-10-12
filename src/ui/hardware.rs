use super::*;
use system_monitor_x::hardware::HardwareInventory;

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
fn affinity_text(values: &[system_monitor_x::cpu_topology::ProcessorAffinity]) -> String {
    values
        .iter()
        .map(|affinity| {
            let processors = affinity
                .logical_processor_numbers()
                .into_iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("组 {}：CPU {processors}", affinity.group)
        })
        .collect::<Vec<_>>()
        .join("；")
}
fn boolean(value: Option<bool>, yes: &str, no: &str) -> String {
    value
        .map(|value| if value { yes } else { no }.to_owned())
        .unwrap_or_else(|| "—".into())
}
fn section(title: impl Into<String>, rows: Vec<(String, String)>) -> HardwareSection {
    HardwareSection {
        title: title.into(),
        rows,
    }
}

/// Format static firmware fields on the inventory worker too. Live UI frames
/// reuse these strings instead of repeating WMI queries or device formatting.
fn build_sections(h: &HardwareInventory) -> Vec<HardwareSection> {
    let mut sections = Vec::new();
    if let Some(system) = &h.system {
        sections.push(section(
            "整机",
            vec![
                (
                    "厂商 / 型号".into(),
                    format!("{} · {}", text(&system.manufacturer), text(&system.model)),
                ),
                ("系统架构".into(), text(&system.system_type)),
            ],
        ));
    }
    if let Some(topology) = &h.cpu_topology {
        let mut rows = vec![
            (
                "拓扑读取状态".into(),
                if topology.complete {
                    "完整"
                } else {
                    "部分可用"
                }
                .into(),
            ),
            (
                "物理插槽 / 核心 / 逻辑处理器".into(),
                format!(
                    "{} / {} / {}",
                    topology.packages.len(),
                    topology.cores.len(),
                    number(topology.logical_processor_count, "")
                ),
            ),
            (
                "NUMA 节点 / 活跃处理器组".into(),
                format!(
                    "{} / {}",
                    topology.numa_nodes.len(),
                    number(topology.active_group_count.map(u32::from), "")
                ),
            ),
            (
                "启用 SMT 的核心".into(),
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
            "Windows 性能 / 能效类（原值）".into(),
            classes
                .into_iter()
                .map(|(class, count)| format!("类 {class}：{count} 核心"))
                .collect::<Vec<_>>()
                .join("；"),
        ));
        for group in &topology.groups {
            rows.push((
                format!("处理器组 {}", group.number),
                format!(
                    "{} 活跃 / {} 上限 · 掩码 0x{:X}",
                    group.active_processor_count,
                    group.maximum_processor_count,
                    group.active_processor_mask
                ),
            ));
        }
        for node in &topology.numa_nodes {
            rows.push((
                format!("NUMA {}", node.node_number),
                affinity_text(&node.affinities),
            ));
        }
        for package in &topology.packages {
            rows.push((
                format!("物理插槽 {}", package.index),
                affinity_text(&package.affinities),
            ));
        }
        sections.push(section("CPU 原生拓扑", rows));
        sections.push(section(
            "CPU 核心与逻辑处理器",
            topology
                .cores
                .iter()
                .map(|core| {
                    (
                        format!(
                            "核心 {} · 插槽 {}",
                            core.index,
                            number(core.package_index, "")
                        ),
                        format!(
                            "{} · 类 {} · {}",
                            if core.smt { "SMT" } else { "单线程核心" },
                            core.efficiency_class,
                            affinity_text(&core.affinities)
                        ),
                    )
                })
                .collect(),
        ));
        sections.push(section(
            "CPU 原生缓存",
            topology
                .caches
                .iter()
                .map(|cache| {
                    (
                        format!("L{} {} · #{}", cache.level, cache.kind, cache.index),
                        format!(
                            "{} · 缓存行 {} B · {} · {}",
                            bytes(cache.size_bytes),
                            cache.line_size_bytes,
                            match cache.associativity {
                                0 => "相联度未提供".to_owned(),
                                255 => "全相联".to_owned(),
                                value => format!("{value} 路相联"),
                            },
                            affinity_text(&cache.affinities)
                        ),
                    )
                })
                .collect(),
        ));
        if !topology.dies.is_empty() {
            sections.push(section(
                "CPU 晶粒",
                topology
                    .dies
                    .iter()
                    .map(|die| {
                        (
                            format!("晶粒 {}", die.index),
                            affinity_text(&die.affinities),
                        )
                    })
                    .collect(),
            ));
        }
        if !topology.modules.is_empty() {
            sections.push(section(
                "CPU 模块",
                topology
                    .modules
                    .iter()
                    .map(|module| {
                        (
                            format!("模块 {}", module.index),
                            affinity_text(&module.affinities),
                        )
                    })
                    .collect(),
            ));
        }
    }
    if let Some(power) = &h.power_status {
        sections.push(section(
            "电源与电池",
            vec![
                (
                    "外接电源".into(),
                    boolean(power.ac_online, "已连接", "未连接"),
                ),
                (
                    "系统电池".into(),
                    boolean(power.battery_present, "已检测到", "无系统电池"),
                ),
                (
                    "电量".into(),
                    power
                        .battery_percent
                        .map(|value| format!("{value}%"))
                        .unwrap_or_else(|| "—".into()),
                ),
                (
                    "充电 / 节电模式".into(),
                    format!(
                        "{} / {}",
                        boolean(power.charging, "充电中", "未充电"),
                        boolean(power.battery_saver, "已启用", "未启用")
                    ),
                ),
                (
                    "预计剩余续航".into(),
                    power
                        .remaining_seconds
                        .map(|value| duration(u64::from(value)))
                        .unwrap_or_else(|| "—".into()),
                ),
                ("状态刷新".into(), "随设备信息刷新；可手动重新读取".into()),
            ],
        ));
    }
    for battery in &h.batteries {
        sections.push(section(
            format!("电池 · {}", battery.device_id),
            vec![
                ("名称 / 型号".into(), text(&battery.name)),
                (
                    "化学类型 / 状态".into(),
                    format!("{} / {}", text(&battery.chemistry), text(&battery.status)),
                ),
                (
                    "电量".into(),
                    battery
                        .estimated_charge_percent
                        .map(|value| format!("{value}%"))
                        .unwrap_or_else(|| "—".into()),
                ),
                (
                    "设计容量 / 满充容量".into(),
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
                    "设计电压".into(),
                    battery
                        .design_voltage_mv
                        .map(|value| format!("{value} mV"))
                        .unwrap_or_else(|| "—".into()),
                ),
                ("设备标识".into(), text(&battery.pnp_device_id)),
            ],
        ));
    }
    for board in &h.motherboards {
        sections.push(section(
            "主板",
            vec![
                (
                    "厂商 / 产品".into(),
                    format!("{} · {}", text(&board.manufacturer), text(&board.product)),
                ),
                ("版本".into(), text(&board.version)),
                ("序列号".into(), text(&board.serial_number)),
            ],
        ));
    }
    for bios in &h.bios {
        sections.push(section(
            "BIOS / 固件",
            vec![
                (
                    "厂商 / 版本".into(),
                    format!("{} · {}", text(&bios.manufacturer), text(&bios.version)),
                ),
                ("发布日期".into(), text(&bios.release_date)),
                ("SMBIOS".into(), text(&bios.smbios_version)),
            ],
        ));
    }
    for cpu in &h.processors {
        sections.push(section(
            format!("处理器 · {}", cpu.device_id),
            vec![
                ("型号".into(), text(&cpu.name)),
                (
                    "插槽 / 厂商".into(),
                    format!("{} · {}", text(&cpu.socket), text(&cpu.manufacturer)),
                ),
                (
                    "核心 / 线程".into(),
                    format!(
                        "{} / {}",
                        number(cpu.physical_cores, ""),
                        number(cpu.logical_processors, "")
                    ),
                ),
                ("标称最高频率".into(), number(cpu.max_clock_mhz, " MHz")),
                (
                    "L2 / L3 缓存".into(),
                    format!(
                        "{} / {}",
                        capacity(cpu.l2_cache_bytes),
                        capacity(cpu.l3_cache_bytes)
                    ),
                ),
                (
                    "固件虚拟化".into(),
                    cpu.virtualization_firmware_enabled
                        .map(|v| if v { "已启用" } else { "未启用" }.to_owned())
                        .unwrap_or_else(|| "不可用".into()),
                ),
            ],
        ));
    }
    if !h.cpu_caches.is_empty() {
        sections.push(section(
            "处理器缓存",
            h.cpu_caches
                .iter()
                .map(|cache| {
                    (
                        format!("{} · {}", cache.device_id, number(cache.level, " 级")),
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
        "内存插槽",
        vec![
            (
                "已用 / 总槽位".into(),
                format!(
                    "{} / {}",
                    h.memory_modules.len(),
                    number(h.memory_slots_total, "")
                ),
            ),
            ("固件最大容量".into(), capacity(h.memory_max_capacity_bytes)),
        ],
    ));
    for (index, memory) in h.memory_modules.iter().enumerate() {
        sections.push(section(
            format!("内存模块 {} · {}", index + 1, text(&memory.device_locator)),
            vec![
                (
                    "容量 / 类型".into(),
                    format!(
                        "{} · {} · {}",
                        capacity(memory.capacity_bytes),
                        text(&memory.memory_type),
                        text(&memory.form_factor)
                    ),
                ),
                (
                    "当前 / 标称速率".into(),
                    format!(
                        "{} / {}",
                        number(memory.configured_speed_mts, " MT/s"),
                        number(memory.rated_speed_mts, " MT/s")
                    ),
                ),
                (
                    "厂商 / 料号".into(),
                    format!(
                        "{} · {}",
                        text(&memory.manufacturer),
                        text(&memory.part_number)
                    ),
                ),
                ("序列号".into(), text(&memory.serial_number)),
                (
                    "数据 / 总线宽度".into(),
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
            format!("物理存储 · {}", disk.device_id),
            vec![
                (
                    "型号 / 容量".into(),
                    format!("{} · {}", text(&disk.model), capacity(disk.capacity_bytes)),
                ),
                (
                    "接口 / 介质".into(),
                    format!("{} · {}", text(&disk.bus_type), text(&disk.media_type)),
                ),
                (
                    "固件 / 序列号".into(),
                    format!(
                        "{} · {}",
                        text(&disk.firmware_version),
                        text(&disk.serial_number)
                    ),
                ),
                (
                    "逻辑 / 物理扇区".into(),
                    format!(
                        "{} / {}",
                        capacity(disk.logical_sector_bytes),
                        capacity(disk.physical_sector_bytes)
                    ),
                ),
                (
                    "健康状态 / 来源".into(),
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
            format!("网卡 · {}", nic.name),
            vec![
                (
                    "硬件 / 状态".into(),
                    format!(
                        "{} · {}",
                        text(&nic.description),
                        text(&nic.connection_state)
                    ),
                ),
                (
                    "MAC / 接口索引".into(),
                    format!(
                        "{} · {}",
                        text(&nic.mac_address),
                        number(nic.interface_index, "")
                    ),
                ),
                (
                    "接收 / 发送链路速率".into(),
                    format!(
                        "{} / {}",
                        link(nic.receive_link_bits_per_sec),
                        link(nic.transmit_link_bits_per_sec)
                    ),
                ),
                (
                    "驱动 / 提供商".into(),
                    format!(
                        "{} · {}",
                        text(&nic.driver_version),
                        text(&nic.driver_provider)
                    ),
                ),
                ("驱动日期".into(), text(&nic.driver_date)),
                ("硬件标识".into(), text(&nic.pnp_device_id)),
            ],
        ));
    }
    for graphics in &h.graphics_adapters {
        sections.push(section(
            format!(
                "{} · {}",
                if graphics.is_pci_device {
                    "显卡驱动"
                } else {
                    "虚拟显示驱动"
                },
                graphics.name
            ),
            vec![
                (
                    "驱动 / 日期".into(),
                    format!(
                        "{} · {}",
                        text(&graphics.driver_version),
                        text(&graphics.driver_date)
                    ),
                ),
                (
                    "当前显示模式".into(),
                    format!(
                        "{} × {} · {}",
                        number(graphics.active_width_pixels, ""),
                        number(graphics.active_height_pixels, ""),
                        number(graphics.active_refresh_hz, " Hz")
                    ),
                ),
                ("硬件标识".into(), text(&graphics.pnp_device_id)),
            ],
        ));
    }
    for monitor in &h.monitors {
        sections.push(section(
            format!("显示器 · {}", text(&monitor.friendly_name)),
            vec![
                (
                    "厂商 / 序列号".into(),
                    format!(
                        "{} · {}",
                        text(&monitor.manufacturer),
                        text(&monitor.serial_number)
                    ),
                ),
                (
                    "生产年 / 周".into(),
                    format!(
                        "{} / {}",
                        number(monitor.manufacture_year, ""),
                        number(monitor.manufacture_week, "")
                    ),
                ),
                ("设备标识".into(), monitor.instance_name.clone()),
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
        cx.spawn(async move |view, cx| {
            let result = smol::unblock(|| {
                HardwareInventory::collect().map(|inventory| {
                    let sections = build_sections(&inventory);
                    (Arc::new(inventory), sections)
                })
            })
            .await;
            let _ = view.update(cx, |this, cx| {
                this.hardware_loading = false;
                match result {
                    Ok((inventory, sections)) => {
                        this.hardware = Some(inventory);
                        this.hardware_sections = sections;
                        this.hardware_error = None;
                    }
                    Err(error) => this.hardware_error = Some(format!("硬件读取失败：{error:#}")),
                }
                cx.notify();
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
                            .child("硬件详细信息"),
                    )
                    .child(
                        Button::new("refresh-hardware")
                            .small()
                            .label(if self.hardware_loading {
                                "读取中…"
                            } else {
                                "刷新硬件"
                            })
                            .disabled(self.hardware_loading)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_hardware(cx))),
                    ),
            )
            .when(self.hardware.is_none() && self.hardware_loading, |e| {
                e.child("正在后台读取固件与驱动信息…")
            })
            .when_some(self.hardware_error.clone(), |e, error| e.child(error))
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
                        .child(format!(
                            "来源：Windows WMI / 固件 · 此次读取 {} ms · 静态信息按需刷新",
                            inventory.collection_duration_ms
                        )),
                )
                .children(
                    inventory
                        .issues
                        .iter()
                        .map(|issue| div().text_xs().child(issue.clone())),
                )
            })
            .into_any_element()
    }
}
