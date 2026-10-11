//! Static Windows hardware inventory. Call `HardwareInventory::collect()` on a
//! dedicated background job at startup or on explicit refresh, never every frame.
//! Queries are read-only and use local WMI providers; one failed device category
//! does not discard successfully collected categories. Missing firmware fields
//! remain `None`. Serial numbers are kept locally and are not automatically sent.

use std::collections::HashMap;
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result};
use wmi::{Variant, WMIConnection};

#[derive(Clone, Debug, Default)]
pub struct SystemHardware {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub system_type: Option<String>,
    pub total_physical_memory_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct MotherboardInfo {
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub version: Option<String>,
    pub serial_number: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct BiosInfo {
    pub manufacturer: Option<String>,
    pub version: Option<String>,
    pub release_date: Option<String>,
    pub smbios_version: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessorInfo {
    pub device_id: String,
    pub name: Option<String>,
    pub manufacturer: Option<String>,
    pub socket: Option<String>,
    pub physical_cores: Option<u32>,
    pub logical_processors: Option<u32>,
    pub max_clock_mhz: Option<u32>,
    pub l2_cache_bytes: Option<u64>,
    pub l3_cache_bytes: Option<u64>,
    pub virtualization_firmware_enabled: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct CpuCacheInfo {
    pub device_id: String,
    pub level: Option<u32>,
    pub size_bytes: Option<u64>,
    pub cache_type: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct MemoryModuleInfo {
    pub device_locator: Option<String>,
    pub bank_label: Option<String>,
    pub capacity_bytes: Option<u64>,
    /// Firmware-reported SMBIOS DDR data rate, in MT/s, not half-rate DRAM clock.
    pub configured_speed_mts: Option<u32>,
    pub rated_speed_mts: Option<u32>,
    pub manufacturer: Option<String>,
    pub part_number: Option<String>,
    pub serial_number: Option<String>,
    pub memory_type: Option<String>,
    pub form_factor: Option<String>,
    pub data_width_bits: Option<u32>,
    pub total_width_bits: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct PhysicalDiskInfo {
    pub device_id: String,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub firmware_version: Option<String>,
    pub serial_number: Option<String>,
    pub capacity_bytes: Option<u64>,
    pub bus_type: Option<String>,
    pub media_type: Option<String>,
    pub physical_sector_bytes: Option<u64>,
    pub logical_sector_bytes: Option<u64>,
    pub health_status: Option<String>,
    /// Provider used for this row; the fallback cannot identify NVMe reliably.
    pub source: String,
}

#[derive(Clone, Debug, Default)]
pub struct NetworkAdapterHardware {
    pub name: String,
    pub description: Option<String>,
    pub interface_index: Option<u32>,
    pub pnp_device_id: Option<String>,
    pub mac_address: Option<String>,
    /// Negotiated link capacity in bits/s; not current bytes/s throughput.
    pub receive_link_bits_per_sec: Option<u64>,
    pub transmit_link_bits_per_sec: Option<u64>,
    pub driver_version: Option<String>,
    pub driver_provider: Option<String>,
    pub driver_date: Option<String>,
    pub driver_path: Option<String>,
    pub is_hardware: Option<bool>,
    pub is_virtual: Option<bool>,
    pub connection_state: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct GraphicsAdapterHardware {
    pub name: String,
    pub pnp_device_id: Option<String>,
    pub vendor_id: Option<u16>,
    pub device_id: Option<u16>,
    pub driver_version: Option<String>,
    pub driver_date: Option<String>,
    pub video_processor: Option<String>,
    pub active_width_pixels: Option<u32>,
    pub active_height_pixels: Option<u32>,
    pub active_refresh_hz: Option<u32>,
    /// Distinguishes the installed virtual displays from physical PCI GPUs.
    pub is_pci_device: bool,
}

#[derive(Clone, Debug, Default)]
pub struct MonitorHardware {
    pub instance_name: String,
    pub active: Option<bool>,
    pub manufacturer: Option<String>,
    pub friendly_name: Option<String>,
    pub serial_number: Option<String>,
    pub manufacture_week: Option<u32>,
    pub manufacture_year: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct HardwareInventory {
    pub collected_at: SystemTime,
    pub collection_duration_ms: u64,
    pub system: Option<SystemHardware>,
    pub motherboards: Vec<MotherboardInfo>,
    pub bios: Vec<BiosInfo>,
    pub processors: Vec<ProcessorInfo>,
    pub cpu_caches: Vec<CpuCacheInfo>,
    pub memory_modules: Vec<MemoryModuleInfo>,
    pub memory_slots_total: Option<u32>,
    pub memory_max_capacity_bytes: Option<u64>,
    pub physical_disks: Vec<PhysicalDiskInfo>,
    pub network_adapters: Vec<NetworkAdapterHardware>,
    pub graphics_adapters: Vec<GraphicsAdapterHardware>,
    pub monitors: Vec<MonitorHardware>,
    pub issues: Vec<String>,
}

impl Default for HardwareInventory {
    fn default() -> Self {
        Self {
            collected_at: SystemTime::now(),
            collection_duration_ms: 0,
            system: None,
            motherboards: Vec::new(),
            bios: Vec::new(),
            processors: Vec::new(),
            cpu_caches: Vec::new(),
            memory_modules: Vec::new(),
            memory_slots_total: None,
            memory_max_capacity_bytes: None,
            physical_disks: Vec::new(),
            network_adapters: Vec::new(),
            graphics_adapters: Vec::new(),
            monitors: Vec::new(),
            issues: Vec::new(),
        }
    }
}

impl HardwareInventory {
    pub fn collect() -> Result<Self> {
        let started = Instant::now();
        let connection = WMIConnection::new().context("无法连接 Windows 硬件信息服务")?;
        let mut inventory = Self::default();

        inventory.system = query(
            &connection,
            "SELECT Manufacturer,Model,SystemType,TotalPhysicalMemory FROM Win32_ComputerSystem",
            "系统型号",
            &mut inventory.issues,
        )
        .first()
        .map(|row| SystemHardware {
            manufacturer: string(row, "Manufacturer"),
            model: string(row, "Model"),
            system_type: string(row, "SystemType"),
            total_physical_memory_bytes: positive(row, "TotalPhysicalMemory"),
        });
        inventory.motherboards = query(
            &connection,
            "SELECT Manufacturer,Product,Version,SerialNumber FROM Win32_BaseBoard",
            "主板",
            &mut inventory.issues,
        )
        .iter()
        .map(|row| MotherboardInfo {
            manufacturer: string(row, "Manufacturer"),
            product: string(row, "Product"),
            version: string(row, "Version"),
            serial_number: string(row, "SerialNumber"),
        })
        .collect();
        inventory.bios = query(&connection,
            "SELECT Manufacturer,SMBIOSBIOSVersion,ReleaseDate,SMBIOSMajorVersion,SMBIOSMinorVersion FROM Win32_BIOS",
            "BIOS", &mut inventory.issues).iter().map(|row| BiosInfo {
                manufacturer: string(row, "Manufacturer"),
                version: string(row, "SMBIOSBIOSVersion"),
                release_date: string(row, "ReleaseDate").and_then(|date| cim_date(&date)),
                smbios_version: number(row, "SMBIOSMajorVersion").zip(number(row, "SMBIOSMinorVersion"))
                    .map(|(major, minor)| format!("{major}.{minor}")),
            }).collect();
        inventory.processors = query(&connection,
            "SELECT DeviceID,Name,Manufacturer,SocketDesignation,NumberOfCores,NumberOfLogicalProcessors,MaxClockSpeed,L2CacheSize,L3CacheSize,VirtualizationFirmwareEnabled FROM Win32_Processor",
            "处理器", &mut inventory.issues).iter().map(|row| ProcessorInfo {
                device_id: string(row, "DeviceID").unwrap_or_default(),
                name: string(row, "Name"),
                manufacturer: string(row, "Manufacturer"),
                socket: string(row, "SocketDesignation"),
                physical_cores: positive_u32(row, "NumberOfCores"),
                logical_processors: positive_u32(row, "NumberOfLogicalProcessors"),
                max_clock_mhz: positive_u32(row, "MaxClockSpeed"),
                l2_cache_bytes: positive(row, "L2CacheSize").and_then(|value| value.checked_mul(1024)),
                l3_cache_bytes: positive(row, "L3CacheSize").and_then(|value| value.checked_mul(1024)),
                virtualization_firmware_enabled: boolean(row, "VirtualizationFirmwareEnabled"),
            }).collect();
        inventory.cpu_caches = query(
            &connection,
            "SELECT DeviceID,Level,InstalledSize,CacheType FROM Win32_CacheMemory",
            "CPU 缓存",
            &mut inventory.issues,
        )
        .iter()
        .map(|row| CpuCacheInfo {
            device_id: string(row, "DeviceID").unwrap_or_default(),
            level: number(row, "Level")
                .filter(|level| (3..=6).contains(level))
                .map(|level| (level - 2) as u32),
            size_bytes: positive(row, "InstalledSize").and_then(|value| value.checked_mul(1024)),
            cache_type: number(row, "CacheType").and_then(|kind| match kind {
                3 => Some("指令".into()),
                4 => Some("数据".into()),
                5 => Some("统一缓存".into()),
                _ => None,
            }),
        })
        .collect();
        inventory.memory_modules = query(&connection,
            "SELECT DeviceLocator,BankLabel,Capacity,ConfiguredClockSpeed,Speed,Manufacturer,PartNumber,SerialNumber,SMBIOSMemoryType,FormFactor,DataWidth,TotalWidth FROM Win32_PhysicalMemory",
            "内存模组", &mut inventory.issues).iter().map(|row| MemoryModuleInfo {
                device_locator: string(row, "DeviceLocator"),
                bank_label: string(row, "BankLabel"),
                capacity_bytes: positive(row, "Capacity"),
                configured_speed_mts: positive_u32(row, "ConfiguredClockSpeed"),
                rated_speed_mts: positive_u32(row, "Speed"),
                manufacturer: string(row, "Manufacturer"),
                part_number: string(row, "PartNumber"),
                serial_number: string(row, "SerialNumber"),
                memory_type: number(row, "SMBIOSMemoryType").and_then(memory_type),
                form_factor: number(row, "FormFactor").and_then(|value| match value {
                    8 => Some("DIMM".into()), 12 => Some("SO-DIMM".into()), 21 => Some("BGA".into()),
                    22 => Some("FPBGA".into()), 23 => Some("LGA".into()), _ => None,
                }),
                data_width_bits: positive_u32(row, "DataWidth"),
                total_width_bits: positive_u32(row, "TotalWidth"),
            }).collect();
        let memory_arrays = query(
            &connection,
            "SELECT MemoryDevices,MaxCapacity,MaxCapacityEx,Use FROM Win32_PhysicalMemoryArray WHERE Use=3",
            "内存插槽",
            &mut inventory.issues,
        );
        inventory.memory_slots_total = checked_sum(
            memory_arrays
                .iter()
                .map(|row| positive(row, "MemoryDevices")),
        )
        .and_then(|value| value.try_into().ok());
        inventory.memory_max_capacity_bytes = checked_sum(memory_arrays.iter().map(|row| {
            positive(row, "MaxCapacityEx")
                .or_else(|| positive(row, "MaxCapacity").and_then(|value| value.checked_mul(1024)))
        }));
        inventory.graphics_adapters = query(&connection,
            "SELECT Name,PNPDeviceID,DriverVersion,DriverDate,VideoProcessor,CurrentHorizontalResolution,CurrentVerticalResolution,CurrentRefreshRate FROM Win32_VideoController",
            "显卡驱动", &mut inventory.issues).iter().map(|row| {
                let pnp_device_id = string(row, "PNPDeviceID");
                let identifiers = pnp_device_id.as_deref().and_then(pci_identifiers);
                GraphicsAdapterHardware {
                    name: string(row, "Name").unwrap_or_else(|| "未命名显示适配器".into()),
                    vendor_id: identifiers.map(|value| value.0),
                    device_id: identifiers.map(|value| value.1),
                    is_pci_device: pnp_device_id.as_ref().is_some_and(|id| id.to_ascii_uppercase().starts_with("PCI\\")),
                    pnp_device_id,
                    driver_version: string(row, "DriverVersion"),
                    driver_date: string(row, "DriverDate").and_then(|date| cim_date(&date)),
                    video_processor: string(row, "VideoProcessor"),
                    active_width_pixels: positive_u32(row, "CurrentHorizontalResolution"),
                    active_height_pixels: positive_u32(row, "CurrentVerticalResolution"),
                    active_refresh_hz: positive_u32(row, "CurrentRefreshRate").filter(|value| *value != 0xFFFF_FFFF && *value != 1),
                }
            }).collect();

        inventory.physical_disks = collect_disks(&connection, &mut inventory.issues);
        inventory.network_adapters = collect_networks(&connection, &mut inventory.issues);
        inventory.monitors = match WMIConnection::with_namespace_path("ROOT\\WMI") {
            Ok(connection) => query(&connection,
                "SELECT InstanceName,Active,ManufacturerName,UserFriendlyName,SerialNumberID,WeekOfManufacture,YearOfManufacture FROM WmiMonitorID",
                "显示器 EDID", &mut inventory.issues).iter().map(|row| MonitorHardware {
                    instance_name: string(row, "InstanceName").unwrap_or_default(),
                    active: boolean(row, "Active"),
                    manufacturer: utf16_array(row, "ManufacturerName"),
                    friendly_name: utf16_array(row, "UserFriendlyName"),
                    serial_number: utf16_array(row, "SerialNumberID"),
                    manufacture_week: positive_u32(row, "WeekOfManufacture").filter(|week| *week <= 53),
                    manufacture_year: positive_u32(row, "YearOfManufacture").filter(|year| (1990..=2200).contains(year)),
                }).collect(),
            Err(error) => { inventory.issues.push(format!("显示器 EDID 服务不可用：{error}")); Vec::new() }
        };
        inventory
            .memory_modules
            .sort_by(|a, b| a.device_locator.cmp(&b.device_locator));
        inventory
            .physical_disks
            .sort_by(|a, b| a.device_id.cmp(&b.device_id));
        inventory
            .network_adapters
            .sort_by(|a, b| a.name.cmp(&b.name));
        inventory.collection_duration_ms =
            started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        inventory.collected_at = SystemTime::now();
        Ok(inventory)
    }
}

type Row = HashMap<String, Variant>;

fn query(
    connection: &WMIConnection,
    sql: &str,
    category: &str,
    issues: &mut Vec<String>,
) -> Vec<Row> {
    match connection.raw_query(sql) {
        Ok(rows) => rows,
        Err(error) => {
            issues.push(format!("{category}读取失败：{error}"));
            Vec::new()
        }
    }
}

fn collect_disks(fallback: &WMIConnection, issues: &mut Vec<String>) -> Vec<PhysicalDiskInfo> {
    if let Ok(connection) = WMIConnection::with_namespace_path("ROOT\\Microsoft\\Windows\\Storage")
    {
        let rows = query(
            &connection,
            "SELECT DeviceId,FriendlyName,Manufacturer,FirmwareVersion,SerialNumber,Size,BusType,MediaType,PhysicalSectorSize,LogicalSectorSize,HealthStatus FROM MSFT_PhysicalDisk",
            "物理磁盘",
            issues,
        );
        if !rows.is_empty() {
            return rows
                .iter()
                .map(|row| PhysicalDiskInfo {
                    device_id: string(row, "DeviceId").unwrap_or_default(),
                    model: string(row, "FriendlyName"),
                    manufacturer: string(row, "Manufacturer"),
                    firmware_version: string(row, "FirmwareVersion"),
                    serial_number: string(row, "SerialNumber"),
                    capacity_bytes: positive(row, "Size"),
                    bus_type: number(row, "BusType").and_then(bus_type),
                    media_type: number(row, "MediaType").and_then(|kind| match kind {
                        3 => Some("HDD".into()),
                        4 => Some("SSD".into()),
                        5 => Some("SCM".into()),
                        _ => None,
                    }),
                    physical_sector_bytes: positive(row, "PhysicalSectorSize"),
                    logical_sector_bytes: positive(row, "LogicalSectorSize"),
                    health_status: number(row, "HealthStatus").and_then(|health| match health {
                        0 => Some("正常".into()),
                        1 => Some("警告".into()),
                        2 => Some("异常".into()),
                        _ => None,
                    }),
                    source: "MSFT_PhysicalDisk".into(),
                })
                .collect();
        }
    } else {
        issues.push("Windows 存储硬件服务不可用，使用磁盘清单兼容接口".into());
    }
    query(fallback,
        "SELECT DeviceID,Model,Manufacturer,FirmwareRevision,SerialNumber,Size,InterfaceType,BytesPerSector,Status FROM Win32_DiskDrive",
        "兼容磁盘清单", issues).iter().map(|row| PhysicalDiskInfo {
            device_id: string(row, "DeviceID").unwrap_or_default(),
            model: string(row, "Model"),
            manufacturer: string(row, "Manufacturer"),
            firmware_version: string(row, "FirmwareRevision"),
            serial_number: string(row, "SerialNumber"),
            capacity_bytes: positive(row, "Size"),
            // Win32_DiskDrive reports NVMe as SCSI; retain its honest provider
            // string rather than guessing the actual physical transport.
            bus_type: string(row, "InterfaceType"),
            logical_sector_bytes: positive(row, "BytesPerSector"),
            health_status: string(row, "Status"),
            source: "Win32_DiskDrive（兼容接口）".into(),
            ..Default::default()
        }).collect()
}

fn collect_networks(
    fallback: &WMIConnection,
    issues: &mut Vec<String>,
) -> Vec<NetworkAdapterHardware> {
    if let Ok(connection) = WMIConnection::with_namespace_path("ROOT\\StandardCimv2") {
        let rows = query(
            &connection,
            "SELECT Name,InterfaceDescription,InterfaceIndex,PnPDeviceID,PermanentAddress,ReceiveLinkSpeed,TransmitLinkSpeed,DriverVersionString,DriverProvider,DriverDate,DriverName,HardwareInterface,Virtual,MediaConnectState FROM MSFT_NetAdapter WHERE HardwareInterface=TRUE",
            "网卡硬件",
            issues,
        );
        if !rows.is_empty() {
            return rows
                .iter()
                .map(|row| NetworkAdapterHardware {
                    name: string(row, "Name").unwrap_or_default(),
                    description: string(row, "InterfaceDescription"),
                    interface_index: number(row, "InterfaceIndex")
                        .and_then(|value| value.try_into().ok()),
                    pnp_device_id: string(row, "PnPDeviceID"),
                    mac_address: string(row, "PermanentAddress"),
                    receive_link_bits_per_sec: positive(row, "ReceiveLinkSpeed"),
                    transmit_link_bits_per_sec: positive(row, "TransmitLinkSpeed"),
                    driver_version: string(row, "DriverVersionString"),
                    driver_provider: string(row, "DriverProvider"),
                    driver_date: string(row, "DriverDate"),
                    driver_path: string(row, "DriverName"),
                    is_hardware: boolean(row, "HardwareInterface"),
                    is_virtual: boolean(row, "Virtual"),
                    connection_state: number(row, "MediaConnectState").and_then(
                        |state| match state {
                            1 => Some("已连接".into()),
                            2 => Some("已断开".into()),
                            _ => None,
                        },
                    ),
                })
                .collect();
        }
    } else {
        issues.push("Windows 网卡硬件服务不可用，使用兼容接口".into());
    }
    query(fallback,
        "SELECT Name,NetConnectionID,InterfaceIndex,PNPDeviceID,MACAddress,Speed,PhysicalAdapter,NetEnabled FROM Win32_NetworkAdapter WHERE PhysicalAdapter=TRUE",
        "兼容网卡清单", issues).iter().map(|row| NetworkAdapterHardware {
            name: string(row, "NetConnectionID").or_else(|| string(row, "Name")).unwrap_or_default(),
            description: string(row, "Name"),
            interface_index: number(row, "InterfaceIndex").and_then(|value| value.try_into().ok()),
            pnp_device_id: string(row, "PNPDeviceID"),
            mac_address: string(row, "MACAddress"),
            receive_link_bits_per_sec: positive(row, "Speed"),
            transmit_link_bits_per_sec: positive(row, "Speed"),
            is_hardware: boolean(row, "PhysicalAdapter"),
            ..Default::default()
        }).collect()
}

fn number(row: &Row, key: &str) -> Option<u64> {
    variant_number(row.get(key)?)
}

fn variant_number(value: &Variant) -> Option<u64> {
    match value {
        Variant::UI1(value) => Some(u64::from(*value)),
        Variant::UI2(value) => Some(u64::from(*value)),
        Variant::UI4(value) => Some(u64::from(*value)),
        Variant::UI8(value) => Some(*value),
        Variant::I1(value) => (*value).try_into().ok(),
        Variant::I2(value) => (*value).try_into().ok(),
        Variant::I4(value) => (*value).try_into().ok(),
        Variant::I8(value) => (*value).try_into().ok(),
        // WMI marshals uint64 values as decimal strings on some providers.
        Variant::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

fn positive(row: &Row, key: &str) -> Option<u64> {
    number(row, key).filter(|value| *value > 0 && *value != u64::MAX)
}

fn positive_u32(row: &Row, key: &str) -> Option<u32> {
    positive(row, key)
        .and_then(|value| value.try_into().ok())
        .filter(|value| *value != u32::MAX)
}

fn string(row: &Row, key: &str) -> Option<String> {
    let Variant::String(value) = row.get(key)? else {
        return None;
    };
    clean_string(value)
}

fn clean_string(value: &str) -> Option<String> {
    let value =
        value.trim_matches(|character: char| character.is_whitespace() || character == '\0');
    if value.is_empty()
        || [
            "unknown",
            "default string",
            "to be filled by o.e.m.",
            "not specified",
            "none",
            "n/a",
        ]
        .iter()
        .any(|missing| value.eq_ignore_ascii_case(missing))
    {
        None
    } else {
        Some(value.to_owned())
    }
}

fn boolean(row: &Row, key: &str) -> Option<bool> {
    match row.get(key)? {
        Variant::Bool(value) => Some(*value),
        _ => None,
    }
}

fn utf16_array(row: &Row, key: &str) -> Option<String> {
    let Variant::Array(values) = row.get(key)? else {
        return None;
    };
    let units = values
        .iter()
        .map(|value| variant_number(value).and_then(|value| value.try_into().ok()))
        .collect::<Option<Vec<u16>>>()?;
    let end = units
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(units.len());
    clean_string(&String::from_utf16_lossy(&units[..end]))
}

fn checked_sum(mut values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    let mut count = 0;
    let sum = values.try_fold(0_u64, |sum, value| {
        count += 1;
        sum.checked_add(value?)
    });
    sum.filter(|_| count > 0)
}

fn cim_date(value: &str) -> Option<String> {
    let prefix = value.get(..8)?;
    if !prefix.bytes().all(|value| value.is_ascii_digit()) {
        return None;
    }
    let (year, month, day) = (&prefix[..4], &prefix[4..6], &prefix[6..8]);
    let month_number = month.parse::<u32>().ok()?;
    let day_number = day.parse::<u32>().ok()?;
    if !(1..=12).contains(&month_number) || !(1..=31).contains(&day_number) {
        return None;
    }
    Some(format!("{year}-{month}-{day}"))
}

fn pci_identifiers(value: &str) -> Option<(u16, u16)> {
    let value = value.to_ascii_uppercase();
    if !value.starts_with("PCI\\") {
        return None;
    }
    let parse = |marker: &str| {
        value
            .split_once(marker)
            .and_then(|(_, tail)| tail.get(..4))
            .and_then(|digits| u16::from_str_radix(digits, 16).ok())
    };
    Some((parse("VEN_")?, parse("DEV_")?))
}

fn memory_type(value: u64) -> Option<String> {
    let name = match value {
        18 => "DDR",
        19 => "DDR2",
        24 => "DDR3",
        26 => "DDR4",
        27 => "LPDDR",
        28 => "LPDDR2",
        29 => "LPDDR3",
        30 => "LPDDR4",
        34 => "DDR5",
        35 => "LPDDR5",
        32 => "HBM",
        33 => "HBM2",
        36 => "HBM3",
        _ => return None,
    };
    Some(name.into())
}

fn bus_type(value: u64) -> Option<String> {
    let name = match value {
        1 => "SCSI",
        2 => "ATAPI",
        3 => "ATA",
        4 => "IEEE 1394",
        5 => "SSA",
        6 => "Fibre Channel",
        7 => "USB",
        8 => "RAID",
        9 => "iSCSI",
        10 => "SAS",
        11 => "SATA",
        12 => "SD",
        13 => "MMC",
        15 => "文件虚拟磁盘",
        16 => "Storage Spaces",
        17 => "NVMe",
        18 => "SCM",
        19 => "UFS",
        _ => return None,
    };
    Some(name.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wmi_integer_strings_do_not_lose_precision_or_fabricate_nulls() {
        assert_eq!(
            variant_number(&Variant::String("34359738368".into())),
            Some(34_359_738_368)
        );
        assert_eq!(variant_number(&Variant::I4(-1)), None);
        assert_eq!(variant_number(&Variant::Null), None);
        assert_eq!(variant_number(&Variant::R8(1.5)), None);
        assert_eq!(checked_sum([Some(1024), None].into_iter()), None);
        assert_eq!(
            checked_sum([Some(1024), Some(2048)].into_iter()),
            Some(3072)
        );
    }

    #[test]
    fn missing_firmware_strings_remain_unavailable() {
        assert_eq!(clean_string("  Unknown \0"), None);
        assert_eq!(clean_string("                            "), None);
        assert_eq!(clean_string("To be filled by O.E.M."), None);
        assert_eq!(
            clean_string("  Ryzen 9 7900X3D  "),
            Some("Ryzen 9 7900X3D".into())
        );
    }

    #[test]
    fn monitor_utf16_and_pci_ids_are_decoded_without_matching_virtual_adapters() {
        let row = [(
            "Name".into(),
            Variant::Array(vec![Variant::UI2(65), Variant::UI2(66), Variant::UI2(0)]),
        )]
        .into_iter()
        .collect();
        assert_eq!(utf16_array(&row, "Name"), Some("AB".into()));
        assert_eq!(
            pci_identifiers("PCI\\VEN_10DE&DEV_2C05&SUBSYS_15017377"),
            Some((0x10de, 0x2c05))
        );
        assert_eq!(pci_identifiers("ROOT\\DISPLAY\\0000"), None);
        assert_eq!(
            cim_date("20240704000000.000000+000"),
            Some("2024-07-04".into())
        );
        assert_eq!(cim_date("**************.******+***"), None);
        assert_eq!(bus_type(17), Some("NVMe".into()));
        assert_eq!(memory_type(34), Some("DDR5".into()));
    }
}
