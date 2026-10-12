//! Read-only native processor topology, collected off the UI thread.
//!
//! The Win32 response is a sequence of variable-length records, not an array
//! of Rust structs. Decode bounded little-endian slices; never dereference a
//! potentially unaligned or truncated union. Unknown records retain metadata
//! and make the result partial instead of manufacturing replacement counts.
//!
//! Sources: Microsoft GetLogicalProcessorInformationEx, PROCESSOR_RELATIONSHIP,
//! NUMA_NODE_RELATIONSHIP, CACHE_RELATIONSHIP and GROUP_RELATIONSHIP docs:
//! https://learn.microsoft.com/windows/win32/api/sysinfoapi/nf-sysinfoapi-getlogicalprocessorinformationex
//! https://learn.microsoft.com/windows/win32/api/winnt/ns-winnt-processor_relationship
//! https://learn.microsoft.com/windows/win32/api/winnt/ns-winnt-numa_node_relationship
//! https://learn.microsoft.com/windows/win32/api/winnt/ns-winnt-cache_relationship
//! https://learn.microsoft.com/windows/win32/api/winnt/ns-winnt-group_relationship

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, RelationAll, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows::core::HRESULT;

const MAX_TOPOLOGY_BYTES: usize = 16 * 1024 * 1024;
const RECORD_HEADER_BYTES: usize = 8;
// Native 64-bit GROUP_AFFINITY: u64 mask, u16 group, three reserved u16s.
const AFFINITY_BYTES: usize = 16;
const PROCESSOR_PREFIX_BYTES: usize = 24;
const NUMA_PREFIX_BYTES: usize = 24;
const CACHE_PREFIX_BYTES: usize = 32;
const GROUP_PREFIX_BYTES: usize = 24;
const GROUP_INFO_BYTES: usize = 48;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessorAffinity {
    pub group: u16,
    pub mask: u64,
}

impl ProcessorAffinity {
    pub fn logical_processor_count(&self) -> u32 {
        self.mask.count_ones()
    }

    /// Numbers are local to the processor group, not global CPU indices.
    pub fn logical_processor_numbers(&self) -> Vec<u8> {
        (0..64)
            .filter(|bit| self.mask & (1_u64 << bit) != 0)
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct CpuCore {
    pub index: u32,
    /// Set only when the core's exact group masks belong to one unique package.
    pub package_index: Option<u32>,
    pub smt: bool,
    /// Windows' raw intrinsic performance/power class, not an Intel P/E label.
    pub efficiency_class: u8,
    pub flags: u8,
    pub affinities: Vec<ProcessorAffinity>,
}

#[derive(Clone, Debug, Default)]
pub struct CpuPackage {
    pub index: u32,
    pub affinities: Vec<ProcessorAffinity>,
}

#[derive(Clone, Debug, Default)]
pub struct NumaNode {
    pub node_number: u32,
    pub affinities: Vec<ProcessorAffinity>,
    /// A GroupMasks array was supplied; false denotes the legacy single mask.
    pub extended: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessorGroup {
    pub number: u16,
    pub maximum_processor_count: u8,
    pub active_processor_count: u8,
    pub active_processor_mask: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CacheKind {
    Unified,
    Instruction,
    Data,
    Trace,
    Unknown(u32),
}

impl fmt::Display for CacheKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unified => formatter.write_str("统一缓存"),
            Self::Instruction => formatter.write_str("指令"),
            Self::Data => formatter.write_str("数据"),
            Self::Trace => formatter.write_str("追踪"),
            Self::Unknown(value) => write!(formatter, "未知类型 {value}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CpuCache {
    pub index: u32,
    pub level: u8,
    pub size_bytes: u64,
    pub line_size_bytes: u16,
    /// Raw documented value: 0xff is fully associative; zero is unspecified.
    pub associativity: u8,
    pub kind: CacheKind,
    pub affinities: Vec<ProcessorAffinity>,
}

#[derive(Clone, Debug)]
pub struct UnknownRelationship {
    pub relationship: u32,
    pub size_bytes: u32,
}

#[derive(Clone, Debug, Default)]
pub struct CpuTopology {
    pub packages: Vec<CpuPackage>,
    pub cores: Vec<CpuCore>,
    pub numa_nodes: Vec<NumaNode>,
    pub groups: Vec<ProcessorGroup>,
    pub caches: Vec<CpuCache>,
    pub dies: Vec<CpuPackage>,
    pub modules: Vec<CpuPackage>,
    pub maximum_group_count: Option<u16>,
    pub active_group_count: Option<u16>,
    /// Available only when the complete group array and its masks agree.
    pub logical_processor_count: Option<u32>,
    pub unknown_relationships: Vec<UnknownRelationship>,
    pub complete: bool,
    pub notes: Vec<String>,
}

impl CpuTopology {
    pub fn collect() -> Result<Self> {
        // WOW64 folds masks for CPUs 32..63. Do not pretend those are complete.
        if std::mem::size_of::<usize>() != 8 {
            bail!("完整 CPU 拓扑需要 64 位采集程序；WOW64 会折叠处理器掩码");
        }
        let mut required = 0_u32;
        // SAFETY: null is the documented size-query buffer; required is writable.
        let query = unsafe { GetLogicalProcessorInformationEx(RelationAll, None, &mut required) };
        if let Err(error) = query
            && error.code() != HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0)
        {
            return Err(error).context("无法查询 CPU 拓扑缓冲区大小");
        }
        for _ in 0..4 {
            let allocation_bytes = usize::try_from(required)?;
            if !(RECORD_HEADER_BYTES..=MAX_TOPOLOGY_BYTES).contains(&allocation_bytes) {
                bail!("CPU 拓扑缓冲区长度异常：{allocation_bytes}");
            }
            // u64 provides the native union alignment; initialized storage also
            // avoids exposing padding as uninitialized Rust memory.
            let mut storage = vec![0_u64; allocation_bytes.div_ceil(8)];
            let mut written = required;
            // SAFETY: storage is aligned, writable, and at least written bytes.
            let result = unsafe {
                GetLogicalProcessorInformationEx(
                    RelationAll,
                    Some(
                        storage
                            .as_mut_ptr()
                            .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>(),
                    ),
                    &mut written,
                )
            };
            match result {
                Ok(()) => {
                    let written = usize::try_from(written)?;
                    if written > allocation_bytes {
                        bail!("CPU 拓扑响应超过请求缓冲区");
                    }
                    // SAFETY: written is checked against the initialized allocation;
                    // the slice is read only, and decoding does not cast any record.
                    let bytes = unsafe {
                        std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), written)
                    };
                    return Ok(parse_records(bytes));
                }
                Err(error) if error.code() == HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0) => {
                    required = written;
                }
                Err(error) => return Err(error).context("读取原生 CPU 拓扑失败"),
            }
        }
        bail!("CPU 拓扑在采集期间持续变化，请重新刷新设备信息")
    }

    fn partial(&mut self, note: impl Into<String>) {
        self.complete = false;
        let note = note.into();
        if self.notes.len() < 128 && !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }
}

fn parse_records(bytes: &[u8]) -> CpuTopology {
    let mut topology = CpuTopology {
        complete: true,
        ..Default::default()
    };
    let mut offset = 0_usize;
    while offset < bytes.len() {
        let tail = &bytes[offset..];
        let (Some(relationship), Some(size)) = (u32_at(tail, 0), u32_at(tail, 4)) else {
            topology.partial(format!("CPU 拓扑记录头截断，偏移 {offset}"));
            break;
        };
        let size_usize = size as usize;
        if size_usize < RECORD_HEADER_BYTES {
            topology.partial(format!("CPU 拓扑记录长度无效：{size}，偏移 {offset}"));
            break;
        }
        let Some(end) = offset
            .checked_add(size_usize)
            .filter(|end| *end <= bytes.len())
        else {
            topology.partial(format!(
                "CPU 拓扑记录越界：关系 {relationship}，长度 {size}"
            ));
            break;
        };
        let payload = &bytes[offset + RECORD_HEADER_BYTES..end];
        match relationship {
            0 | 3 | 5 | 7 => parse_processor(payload, relationship, &mut topology),
            1 | 6 => parse_numa(payload, relationship, &mut topology),
            2 => parse_cache(payload, &mut topology),
            4 => parse_groups(payload, &mut topology),
            _ => {
                topology.unknown_relationships.push(UnknownRelationship {
                    relationship,
                    size_bytes: size,
                });
                topology.partial(format!(
                    "未知 CPU 拓扑关系 {relationship}（{size} 字节），已跳过"
                ));
            }
        }
        offset = end;
    }
    finalize(&mut topology);
    topology
}

fn parse_processor(payload: &[u8], relationship: u32, topology: &mut CpuTopology) {
    if payload.len() < PROCESSOR_PREFIX_BYTES {
        topology.partial(format!("处理器关系 {relationship} 的固定字段截断"));
        return;
    }
    let flags = payload[0];
    let efficiency_class = payload[1];
    let group_count = u16_at(payload, 22).unwrap_or(0);
    let affinities = parse_affinities(payload, PROCESSOR_PREFIX_BYTES, group_count, topology);
    if relationship == 0 && group_count != 1 {
        topology.partial(format!("核心关系声明了 {group_count} 个处理器组，预期为 1"));
    }
    if flags & !1 != 0 {
        topology.partial(format!(
            "处理器关系 {relationship} 含未知 flags 0x{flags:02x}"
        ));
    }
    match relationship {
        0 => topology.cores.push(CpuCore {
            index: topology.cores.len() as u32,
            package_index: None,
            smt: flags & 1 != 0,
            efficiency_class,
            flags,
            affinities,
        }),
        3 => topology.packages.push(CpuPackage {
            index: topology.packages.len() as u32,
            affinities,
        }),
        5 => topology.dies.push(CpuPackage {
            index: topology.dies.len() as u32,
            affinities,
        }),
        7 => topology.modules.push(CpuPackage {
            index: topology.modules.len() as u32,
            affinities,
        }),
        _ => unreachable!(),
    }
}

fn parse_numa(payload: &[u8], relationship: u32, topology: &mut CpuTopology) {
    if payload.len() < NUMA_PREFIX_BYTES {
        topology.partial("NUMA 关系固定字段截断");
        return;
    }
    let node_number = u32_at(payload, 0).unwrap_or(0);
    let reported_groups = u16_at(payload, 22).unwrap_or(0);
    // On pre-20H2 Windows this field was reserved and is zero: one legacy mask.
    let group_count = if reported_groups == 0 && relationship == 1 {
        1
    } else {
        reported_groups
    };
    let affinities = parse_affinities(payload, NUMA_PREFIX_BYTES, group_count, topology);
    let extended = relationship == 6 || reported_groups > 0;
    if let Some(node) = topology
        .numa_nodes
        .iter_mut()
        .find(|node| node.node_number == node_number)
    {
        // RelationAll may include both legacy and extended records. Merge by
        // real node and group; duplicate masks must not double-count CPUs.
        for affinity in affinities {
            if let Some(existing) = node
                .affinities
                .iter_mut()
                .find(|item| item.group == affinity.group)
            {
                existing.mask |= affinity.mask;
            } else {
                node.affinities.push(affinity);
            }
        }
        node.extended |= extended;
    } else {
        topology.numa_nodes.push(NumaNode {
            node_number,
            affinities,
            extended,
        });
    }
}

fn parse_cache(payload: &[u8], topology: &mut CpuTopology) {
    if payload.len() < CACHE_PREFIX_BYTES {
        topology.partial("缓存关系固定字段截断");
        return;
    }
    let kind_value = u32_at(payload, 8).unwrap_or(u32::MAX);
    let kind = match kind_value {
        0 => CacheKind::Unified,
        1 => CacheKind::Instruction,
        2 => CacheKind::Data,
        3 => CacheKind::Trace,
        value => {
            topology.partial(format!("缓存关系含未知类型 {value}"));
            CacheKind::Unknown(value)
        }
    };
    let reported_groups = u16_at(payload, 30).unwrap_or(0);
    // Older CACHE_RELATIONSHIP had Reserved[20] and a single GroupMask.
    let affinities = parse_affinities(
        payload,
        CACHE_PREFIX_BYTES,
        reported_groups.max(1),
        topology,
    );
    let size_bytes = u64::from(u32_at(payload, 4).unwrap_or(0));
    let line_size_bytes = u16_at(payload, 2).unwrap_or(0);
    if payload[0] == 0 || size_bytes == 0 || line_size_bytes == 0 {
        topology.partial("缓存层级、容量或缓存行长度未提供");
    }
    topology.caches.push(CpuCache {
        index: topology.caches.len() as u32,
        level: payload[0],
        size_bytes,
        line_size_bytes,
        associativity: payload[1],
        kind,
        affinities,
    });
}

fn parse_groups(payload: &[u8], topology: &mut CpuTopology) {
    if payload.len() < GROUP_PREFIX_BYTES {
        topology.partial("处理器组关系固定字段截断");
        return;
    }
    if topology.active_group_count.is_some() {
        topology.partial("收到重复的处理器组关系，保留首条");
        return;
    }
    let maximum = u16_at(payload, 0).unwrap_or(0);
    let active = u16_at(payload, 2).unwrap_or(0);
    topology.maximum_group_count = Some(maximum);
    topology.active_group_count = Some(active);
    if active == 0 || active > maximum {
        topology.partial("处理器组活动/最大数量无效");
    }
    let available = (payload.len() - GROUP_PREFIX_BYTES) / GROUP_INFO_BYTES;
    if available < usize::from(active) {
        topology.partial(format!(
            "处理器组数组截断：声明 {active}，有效记录 {available}"
        ));
    }
    for index in 0..usize::from(active).min(available) {
        let start = GROUP_PREFIX_BYTES + index * GROUP_INFO_BYTES;
        let group = &payload[start..start + GROUP_INFO_BYTES];
        let maximum_processor_count = group[0];
        let active_processor_count = group[1];
        let active_processor_mask = u64_at(group, 40).unwrap_or(0);
        if maximum_processor_count > 64
            || active_processor_count > maximum_processor_count
            || u32::from(active_processor_count) != active_processor_mask.count_ones()
        {
            topology.partial(format!("处理器组 {index} 的数量与掩码不一致"));
        }
        topology.groups.push(ProcessorGroup {
            number: index as u16,
            maximum_processor_count,
            active_processor_count,
            active_processor_mask,
        });
    }
}

fn parse_affinities(
    payload: &[u8],
    start: usize,
    count: u16,
    topology: &mut CpuTopology,
) -> Vec<ProcessorAffinity> {
    let available = payload.len().saturating_sub(start) / AFFINITY_BYTES;
    if count == 0 || available < usize::from(count) {
        topology.partial(format!(
            "处理器掩码数组不完整：声明 {count}，有效记录 {available}"
        ));
    }
    let mut affinities = Vec::with_capacity(usize::from(count).min(available));
    let mut groups = BTreeSet::new();
    for index in 0..usize::from(count).min(available) {
        let offset = start + index * AFFINITY_BYTES;
        let mask = u64_at(payload, offset).unwrap_or(0);
        let group = u16_at(payload, offset + 8).unwrap_or(0);
        if mask == 0 || !groups.insert(group) {
            topology.partial(format!("处理器组 {group} 掩码为空或在同一关系中重复"));
        }
        affinities.push(ProcessorAffinity { group, mask });
    }
    affinities
}

fn finalize(topology: &mut CpuTopology) {
    if topology.cores.is_empty() || topology.packages.is_empty() || topology.groups.is_empty() {
        topology.partial("CPU 拓扑缺少核心、物理包或处理器组记录");
    }
    for index in 0..topology.cores.len() {
        let core = &topology.cores[index];
        let matches: Vec<_> = topology
            .packages
            .iter()
            .filter(|package| {
                !core.affinities.is_empty()
                    && core.affinities.iter().all(|affinity| {
                        affinity.mask != 0
                            && package.affinities.iter().any(|item| {
                                item.group == affinity.group
                                    && item.mask & affinity.mask == affinity.mask
                            })
                    })
            })
            .map(|package| package.index)
            .collect();
        let package_index = match matches.as_slice() {
            [package] => Some(*package),
            _ => {
                topology.partial(format!("核心 {index} 无法唯一匹配物理处理器包"));
                None
            }
        };
        topology.cores[index].package_index = package_index;
    }
    let mut coverage = BTreeMap::<u16, u64>::new();
    let mut overlapping = false;
    for core in &topology.cores {
        for affinity in &core.affinities {
            let entry = coverage.entry(affinity.group).or_default();
            overlapping |= *entry & affinity.mask != 0;
            *entry |= affinity.mask;
        }
    }
    if overlapping {
        topology.partial("不同核心的逻辑处理器掩码发生重叠");
    }
    let mut group_counts_valid = topology
        .active_group_count
        .zip(topology.maximum_group_count)
        .is_some_and(|(active, maximum)| {
            active > 0 && active <= maximum && usize::from(active) == topology.groups.len()
        });
    for index in 0..topology.groups.len() {
        let group = &topology.groups[index];
        if coverage.remove(&group.number).unwrap_or(0) != group.active_processor_mask {
            topology.partial(format!("处理器组 {index} 的核心覆盖与活动掩码不一致"));
        }
        let group = &topology.groups[index];
        group_counts_valid &= group.active_processor_count <= group.maximum_processor_count
            && group.maximum_processor_count <= 64
            && u32::from(group.active_processor_count) == group.active_processor_mask.count_ones();
    }
    if !coverage.is_empty() {
        topology.partial("核心引用了未提供的处理器组");
    }
    if group_counts_valid {
        topology.logical_processor_count = Some(
            topology
                .groups
                .iter()
                .map(|group| u32::from(group.active_processor_count))
                .sum(),
        );
    }
    topology.numa_nodes.sort_by_key(|node| node.node_number);
    for node in &mut topology.numa_nodes {
        node.affinities.sort_by_key(|affinity| affinity.group);
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(relationship: u32, payload: &[u8]) -> Vec<u8> {
        let mut bytes = relationship.to_le_bytes().to_vec();
        bytes.extend_from_slice(&((payload.len() + RECORD_HEADER_BYTES) as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn affinity(group: u16, mask: u64) -> Vec<u8> {
        let mut bytes = mask.to_le_bytes().to_vec();
        bytes.extend_from_slice(&group.to_le_bytes());
        bytes.resize(AFFINITY_BYTES, 0);
        bytes
    }

    fn processor(relationship: u32, class: u8, masks: &[(u16, u64)]) -> Vec<u8> {
        let mut payload = vec![0; PROCESSOR_PREFIX_BYTES];
        payload[0] = u8::from(
            relationship == 0 && masks.iter().map(|(_, mask)| mask.count_ones()).sum::<u32>() > 1,
        );
        payload[1] = class;
        payload[22..24].copy_from_slice(&(masks.len() as u16).to_le_bytes());
        for (group, mask) in masks {
            payload.extend(affinity(*group, *mask));
        }
        record(relationship, &payload)
    }

    fn group_record(masks: &[u64]) -> Vec<u8> {
        let mut payload = vec![0; GROUP_PREFIX_BYTES];
        payload[0..2].copy_from_slice(&(masks.len() as u16).to_le_bytes());
        payload[2..4].copy_from_slice(&(masks.len() as u16).to_le_bytes());
        for mask in masks {
            let mut group = vec![0; GROUP_INFO_BYTES];
            group[0] = 64;
            group[1] = mask.count_ones() as u8;
            group[40..48].copy_from_slice(&mask.to_le_bytes());
            payload.extend(group);
        }
        record(4, &payload)
    }

    fn complete_fixture() -> Vec<u8> {
        [
            processor(3, 0, &[(0, 3), (1, 1)]),
            processor(0, 0, &[(0, 3)]),
            processor(0, 7, &[(1, 1)]),
            group_record(&[3, 1]),
        ]
        .concat()
    }

    #[test]
    fn groups_masks_and_efficiency_classes_are_real_fields_not_pe_labels() {
        let topology = parse_records(&complete_fixture());
        assert!(topology.complete, "{:?}", topology.notes);
        assert_eq!(topology.logical_processor_count, Some(3));
        assert_eq!(topology.active_group_count, Some(2));
        assert_eq!(topology.cores[0].package_index, Some(0));
        assert!(topology.cores[0].smt);
        assert_eq!(topology.cores[1].efficiency_class, 7);
        assert_eq!(
            topology.cores[1].affinities[0].logical_processor_numbers(),
            vec![0]
        );
        assert_eq!(topology.groups[1].active_processor_mask, 1);
    }

    #[test]
    fn unknown_records_and_truncated_tail_preserve_preceding_topology() {
        let mut bytes = complete_fixture();
        bytes.extend(record(333, &[1, 2, 3, 4]));
        bytes.extend([0, 1, 2]);
        let topology = parse_records(&bytes);
        assert!(!topology.complete);
        assert_eq!(topology.cores.len(), 2);
        assert_eq!(topology.unknown_relationships[0].relationship, 333);
        assert_eq!(topology.unknown_relationships[0].size_bytes, 12);
        assert!(
            topology
                .notes
                .iter()
                .any(|note| note.contains("记录头截断"))
        );
    }

    #[test]
    fn zero_size_oversize_and_short_payload_never_loop_or_read_out_of_bounds() {
        for tail in [
            record(0, &[]),
            [0_u32.to_le_bytes(), 0_u32.to_le_bytes()].concat(),
            [2_u32.to_le_bytes(), u32::MAX.to_le_bytes()].concat(),
        ] {
            let topology = parse_records(&[complete_fixture(), tail].concat());
            assert!(!topology.complete);
            assert_eq!(topology.cores.len(), 2);
        }
        for length in 0..200 {
            let _ = parse_records(&vec![0xFF; length]);
        }
    }

    #[test]
    fn partial_affinity_arrays_keep_valid_entries_and_cannot_fabricate_counts() {
        let mut package = vec![0; PROCESSOR_PREFIX_BYTES];
        package[22..24].copy_from_slice(&u16::MAX.to_le_bytes());
        package.extend(affinity(1, 8));
        let topology = parse_records(&record(3, &package));
        assert!(!topology.complete);
        assert_eq!(
            topology.packages[0].affinities,
            vec![ProcessorAffinity { group: 1, mask: 8 }]
        );
        assert_eq!(topology.logical_processor_count, None);
    }

    #[test]
    fn legacy_and_extended_numa_merge_by_node_and_group_without_double_counting() {
        let mut legacy = vec![0; NUMA_PREFIX_BYTES];
        legacy[0..4].copy_from_slice(&9_u32.to_le_bytes());
        legacy.extend(affinity(0, 3));
        let mut extended = vec![0; NUMA_PREFIX_BYTES];
        extended[0..4].copy_from_slice(&9_u32.to_le_bytes());
        extended[22..24].copy_from_slice(&2_u16.to_le_bytes());
        extended.extend(affinity(0, 3));
        extended.extend(affinity(1, 1));
        let topology =
            parse_records(&[complete_fixture(), record(1, &legacy), record(6, &extended)].concat());
        assert!(topology.complete, "{:?}", topology.notes);
        assert_eq!(topology.numa_nodes.len(), 1);
        assert_eq!(topology.numa_nodes[0].affinities.len(), 2);
        assert!(topology.numa_nodes[0].extended);
    }

    #[test]
    fn cache_associativity_units_and_unknown_type_are_preserved() {
        let mut cache = vec![0; CACHE_PREFIX_BYTES];
        cache[0] = 3;
        cache[1] = 255;
        cache[2..4].copy_from_slice(&64_u16.to_le_bytes());
        cache[4..8].copy_from_slice(&33_554_432_u32.to_le_bytes());
        cache[8..12].copy_from_slice(&77_u32.to_le_bytes());
        cache.extend(affinity(0, 3)); // legacy GroupCount = 0
        let topology = parse_records(&[complete_fixture(), record(2, &cache)].concat());
        assert!(!topology.complete);
        assert_eq!(topology.caches[0].kind, CacheKind::Unknown(77));
        assert_eq!(topology.caches[0].size_bytes, 33_554_432);
        assert_eq!(topology.caches[0].line_size_bytes, 64);
        assert_eq!(topology.caches[0].associativity, 255);
        assert_eq!(topology.caches[0].affinities[0].mask, 3);
    }

    #[test]
    fn invalid_group_counts_do_not_report_a_complete_cpu_total() {
        let mut bytes = complete_fixture();
        let last_record =
            bytes.len() - (RECORD_HEADER_BYTES + GROUP_PREFIX_BYTES + GROUP_INFO_BYTES * 2);
        bytes[last_record + RECORD_HEADER_BYTES + GROUP_PREFIX_BYTES + 1] = 64;
        let topology = parse_records(&bytes);
        assert!(!topology.complete);
        assert_eq!(topology.logical_processor_count, None);
    }

    #[test]
    fn native_topology_smoke_reads_consistent_core_and_group_masks() {
        let topology = CpuTopology::collect().expect("read-only native topology query");
        assert!(!topology.cores.is_empty());
        assert!(!topology.packages.is_empty());
        assert!(!topology.groups.is_empty());
        assert!(
            topology
                .logical_processor_count
                .is_some_and(|count| count > 0)
        );
        assert!(
            topology
                .cores
                .iter()
                .all(|core| !core.affinities.is_empty())
        );
        for group in &topology.groups {
            assert_eq!(
                u32::from(group.active_processor_count),
                group.active_processor_mask.count_ones()
            );
        }
        println!(
            "native topology: packages={} cores={} logical={:?} groups={} numa={} caches={} complete={} notes={:?}",
            topology.packages.len(),
            topology.cores.len(),
            topology.logical_processor_count,
            topology.groups.len(),
            topology.numa_nodes.len(),
            topology.caches.len(),
            topology.complete,
            topology.notes
        );
    }

    #[test]
    fn zero_declared_active_groups_do_not_become_a_zero_cpu_measurement() {
        let topology = parse_records(&group_record(&[]));
        assert!(!topology.complete);
        assert_eq!(topology.logical_processor_count, None);
    }

    #[test]
    fn documented_x64_offsets_match_the_linked_windows_sdk_bindings() {
        use std::mem::{offset_of, size_of};
        use windows::Win32::System::SystemInformation::{
            CACHE_RELATIONSHIP, GROUP_AFFINITY, GROUP_RELATIONSHIP, NUMA_NODE_RELATIONSHIP,
            PROCESSOR_GROUP_INFO, PROCESSOR_RELATIONSHIP,
        };
        assert_eq!(size_of::<GROUP_AFFINITY>(), AFFINITY_BYTES);
        assert_eq!(
            offset_of!(PROCESSOR_RELATIONSHIP, GroupMask),
            PROCESSOR_PREFIX_BYTES
        );
        assert_eq!(
            offset_of!(NUMA_NODE_RELATIONSHIP, Anonymous),
            NUMA_PREFIX_BYTES
        );
        assert_eq!(
            offset_of!(CACHE_RELATIONSHIP, Anonymous),
            CACHE_PREFIX_BYTES
        );
        assert_eq!(
            offset_of!(GROUP_RELATIONSHIP, GroupInfo),
            GROUP_PREFIX_BYTES
        );
        assert_eq!(size_of::<PROCESSOR_GROUP_INFO>(), GROUP_INFO_BYTES);
        assert_eq!(offset_of!(PROCESSOR_GROUP_INFO, ActiveProcessorMask), 40);
    }
}
