//! Read-only owner-PID TCP/UDP port inventory. Run collection on a worker.
//!
//! IP Helper tables do not contain process creation times and are not atomic
//! with process enumeration. Query each owner's exact FILETIME, reject owners
//! born after the table query began, and merge cached metadata only on exact
//! identity. A later process action must independently verify that FILETIME.
//! https://learn.microsoft.com/windows/win32/api/iphlpapi/nf-iphlpapi-getextendedtcptable
//! https://learn.microsoft.com/windows/win32/api/iphlpapi/nf-iphlpapi-getextendedudptable
//! https://learn.microsoft.com/windows/win32/api/tcpmib/ns-tcpmib-mib_tcp6row_owner_pid
//! https://learn.microsoft.com/windows/win32/api/udpmib/ns-udpmib-mib_udp6row_owner_pid

use crate::monitor::ProcessSample;
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashMap,
    ffi::c_void,
    fmt,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    path::PathBuf,
    time::{Instant, SystemTime},
};
use windows::Win32::{
    Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, FILETIME, HANDLE},
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable, TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
    },
    Networking::WinSock::{AF_INET, AF_INET6},
    System::{
        SystemInformation::GetSystemTimePreciseAsFileTime,
        Threading::{
            GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    },
};
use windows::core::PWSTR;

const MAX_TABLE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TABLE_ROWS: usize = 250_000;
pub const PORT_QUERY_HELP: &str = "搜索名称/地址，或 pid:123 port:8080 local:8080 remote:443 tcp udp ipv4 ipv6；裸数字匹配 PID 或端口，多条件以空格分隔";
pub const PORT_IDENTITY_NOTE: &str = "端口表与进程身份不是原子快照；无法验证身份时禁止结束进程，操作时再次核验精确创建时间。UDP 仅提供绑定端点，未提供远端或连接状态。";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PortProtocol {
    Tcp,
    Udp,
}
impl fmt::Display for PortProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IpFamily {
    V4,
    V6,
}
impl fmt::Display for IpFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::V4 => "IPv4",
            Self::V6 => "IPv6",
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    DeleteTcb,
    Unknown(u32),
}
impl TcpState {
    fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::Closed,
            2 => Self::Listen,
            3 => Self::SynSent,
            4 => Self::SynReceived,
            5 => Self::Established,
            6 => Self::FinWait1,
            7 => Self::FinWait2,
            8 => Self::CloseWait,
            9 => Self::Closing,
            10 => Self::LastAck,
            11 => Self::TimeWait,
            12 => Self::DeleteTcb,
            value => Self::Unknown(value),
        }
    }
}
impl fmt::Display for TcpState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Closed => "CLOSED",
            Self::Listen => "LISTEN",
            Self::SynSent => "SYN-SENT",
            Self::SynReceived => "SYN-RECEIVED",
            Self::Established => "ESTABLISHED",
            Self::FinWait1 => "FIN-WAIT-1",
            Self::FinWait2 => "FIN-WAIT-2",
            Self::CloseWait => "CLOSE-WAIT",
            Self::Closing => "CLOSING",
            Self::LastAck => "LAST-ACK",
            Self::TimeWait => "TIME-WAIT",
            Self::DeleteTcb => "DELETE-TCB",
            Self::Unknown(value) => return write!(f, "UNKNOWN({value})"),
        })
    }
}

#[derive(Clone, Debug)]
pub struct PortEntry {
    pub pid: u32,
    /// Exact creation FILETIME; None disables process actions. Never Unix seconds.
    pub creation_ticks: Option<u64>,
    pub process_name: Option<String>,
    pub exe: Option<PathBuf>,
    pub protocol: PortProtocol,
    pub family: IpFamily,
    pub local: SocketAddr,
    /// None for UDP and TCP listeners; their owner tables do not define a peer.
    pub remote: Option<SocketAddr>,
    pub tcp_state: Option<TcpState>,
    pub identity_note: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PortKey {
    pub pid: u32,
    pub creation_ticks: Option<u64>,
    pub protocol: PortProtocol,
    pub family: IpFamily,
    pub local: SocketAddr,
    pub remote: Option<SocketAddr>,
}
impl PortEntry {
    pub fn key(&self) -> PortKey {
        PortKey {
            pid: self.pid,
            creation_ticks: self.creation_ticks,
            protocol: self.protocol,
            family: self.family,
            local: self.local,
            remote: self.remote,
        }
    }
    pub fn process_identity(&self) -> Option<(u32, u64)> {
        self.creation_ticks
            .filter(|ticks| *ticks > 0)
            .map(|ticks| (self.pid, ticks))
    }
    pub fn can_end_process(&self) -> bool {
        self.pid > 4 && self.pid != std::process::id() && self.process_identity().is_some()
    }
}
#[derive(Clone, Debug)]
pub struct PortSnapshot {
    pub entries: Vec<PortEntry>,
    pub issues: Vec<String>,
    pub collected_at: SystemTime,
    pub collection_duration_ms: u64,
    /// Four endpoint-table reads succeeded; this is not a global atomic snapshot.
    pub complete: bool,
}
impl Default for PortSnapshot {
    fn default() -> Self {
        Self {
            entries: vec![],
            issues: vec![],
            collected_at: SystemTime::now(),
            collection_duration_ms: 0,
            complete: false,
        }
    }
}
impl PortSnapshot {
    pub fn collect(processes: &[ProcessSample]) -> Self {
        let began = Instant::now();
        let mut snapshot = Self {
            complete: true,
            ..Default::default()
        };
        let mut table_beginnings = HashMap::<u32, u64>::new();
        for (protocol, family) in [
            (PortProtocol::Tcp, IpFamily::V4),
            (PortProtocol::Tcp, IpFamily::V6),
            (PortProtocol::Udp, IpFamily::V4),
            (PortProtocol::Udp, IpFamily::V6),
        ] {
            // SAFETY: read-only time query supported by the minimum Windows version.
            let table_start = filetime_ticks(unsafe { GetSystemTimePreciseAsFileTime() });
            match collect_table(protocol, family) {
                Ok(entries) => {
                    for entry in &entries {
                        table_beginnings
                            .entry(entry.pid)
                            .and_modify(|ticks| *ticks = (*ticks).min(table_start))
                            .or_insert(table_start);
                    }
                    snapshot.entries.extend(entries);
                }
                Err(error) => {
                    snapshot.complete = false;
                    snapshot
                        .issues
                        .push(format!("{protocol} {family} 端口表不可用：{error:#}"));
                }
            }
        }
        let known: HashMap<_, _> = processes
            .iter()
            .map(|process| (process.pid, process))
            .collect();
        let owners: HashMap<_, _> = table_beginnings
            .into_iter()
            .map(|(pid, table_start)| {
                (
                    pid,
                    read_owner(pid, table_start, known.get(&pid).copied())
                        .map_err(|error| format!("身份不可验证：{error:#}")),
                )
            })
            .collect();
        for entry in &mut snapshot.entries {
            match owners.get(&entry.pid) {
                Some(Ok(owner)) => {
                    entry.creation_ticks = Some(owner.creation_ticks);
                    entry.process_name = owner.name.clone();
                    entry.exe = owner.exe.clone();
                }
                Some(Err(note)) => entry.identity_note = Some(note.clone()),
                None => entry.identity_note = Some("未获得进程身份".into()),
            }
        }
        snapshot.collected_at = SystemTime::now();
        snapshot.collection_duration_ms =
            began.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        snapshot
    }
}

struct OwnedProcess(HANDLE);
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
struct Owner {
    creation_ticks: u64,
    name: Option<String>,
    exe: Option<PathBuf>,
}
fn read_owner(pid: u32, table_start: u64, known: Option<&ProcessSample>) -> Result<Owner> {
    ensure!(pid > 0, "系统未提供端点所有者 PID");
    // SAFETY: read-only, minimal-rights handle owned by the RAII wrapper.
    let handle = OwnedProcess(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .context("进程已退出、拒绝访问或受到系统保护")?,
    );
    let creation_ticks = active_creation_ticks(handle.0)?;
    ensure!(
        creation_ticks <= table_start,
        "进程出生晚于端口查询开始，可能发生 PID 重用，请刷新"
    );
    let matching = known.filter(|process| process.start_time_windows_ticks == Some(creation_ticks));
    let exe = matching
        .and_then(|process| process.exe.clone())
        .or_else(|| process_path(handle.0).ok());
    let name = matching
        .map(|process| process.name.clone())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            exe.as_ref()?
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
    ensure!(
        active_creation_ticks(handle.0)? == creation_ticks,
        "进程在端口身份读取期间已经变化"
    );
    Ok(Owner {
        creation_ticks,
        name,
        exe,
    })
}
fn active_creation_ticks(handle: HANDLE) -> Result<u64> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: handle remains open and all output structures are writable.
    unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
        .context("无法读取精确进程创建时间")?;
    let creation = filetime_ticks(created);
    ensure!(
        creation > 0 && filetime_ticks(exited) == 0,
        "进程创建时间缺失或进程已经退出"
    );
    Ok(creation)
}
fn filetime_ticks(time: FILETIME) -> u64 {
    u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime)
}
fn process_path(handle: HANDLE) -> Result<PathBuf> {
    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    // SAFETY: initialized UTF-16 storage and accurate writable capacity.
    unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .context("无法读取进程路径")?;
    ensure!(
        length > 0 && length as usize <= buffer.len(),
        "进程路径响应长度异常"
    );
    Ok(PathBuf::from(String::from_utf16_lossy(
        &buffer[..length as usize],
    )))
}
fn collect_table(protocol: PortProtocol, family: IpFamily) -> Result<Vec<PortEntry>> {
    let af = match family {
        IpFamily::V4 => u32::from(AF_INET.0),
        IpFamily::V6 => u32::from(AF_INET6.0),
    };
    let bytes = fetch_table(|buffer, length| {
        // SAFETY: fetch_table provides aligned output storage of *length bytes,
        // Reserved=0 and documented OWNER_PID class for the given address family.
        unsafe {
            match protocol {
                PortProtocol::Tcp => {
                    GetExtendedTcpTable(buffer, length, false, af, TCP_TABLE_OWNER_PID_ALL, 0)
                }
                PortProtocol::Udp => {
                    GetExtendedUdpTable(buffer, length, false, af, UDP_TABLE_OWNER_PID, 0)
                }
            }
        }
    })?;
    parse_table(&bytes, protocol, family)
}
fn fetch_table(mut query: impl FnMut(Option<*mut c_void>, &mut u32) -> u32) -> Result<Vec<u8>> {
    let mut needed = 0_u32;
    let status = query(None, &mut needed);
    ensure!(
        status == 0 || status == ERROR_INSUFFICIENT_BUFFER.0,
        "Win32 错误 {status}"
    );
    for _ in 0..4 {
        let allocated = needed as usize;
        ensure!(
            (4..=MAX_TABLE_BYTES).contains(&allocated),
            "端口表缓冲区大小异常：{allocated}"
        );
        let mut storage = vec![0_u64; allocated.div_ceil(8)];
        let mut written = needed;
        let status = query(Some(storage.as_mut_ptr().cast()), &mut written);
        if status == ERROR_INSUFFICIENT_BUFFER.0 {
            needed = written;
            continue;
        }
        ensure!(status == 0, "Win32 错误 {status}");
        ensure!(
            written as usize <= allocated && written >= 4,
            "端口表返回长度异常：{written}"
        );
        // SAFETY: storage is initialized, alive, and the returned length was bounded.
        return Ok(unsafe {
            std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), written as usize)
        }
        .to_vec());
    }
    bail!("端口表在采集期间持续变化，请重新刷新")
}
fn parse_table(bytes: &[u8], protocol: PortProtocol, family: IpFamily) -> Result<Vec<PortEntry>> {
    let count = u32_at(bytes, 0)? as usize;
    ensure!(count <= MAX_TABLE_ROWS, "端口表行数超过保护上限：{count}");
    let row_size = match (protocol, family) {
        (PortProtocol::Tcp, IpFamily::V4) => 24,
        (PortProtocol::Tcp, IpFamily::V6) => 56,
        (PortProtocol::Udp, IpFamily::V4) => 12,
        (PortProtocol::Udp, IpFamily::V6) => 28,
    };
    let required = count
        .checked_mul(row_size)
        .and_then(|bytes| bytes.checked_add(4))
        .context("端口表长度溢出")?;
    ensure!(
        required <= bytes.len(),
        "端口表数据截断：声明 {count} 行，需 {required} 字节，实际 {}",
        bytes.len()
    );
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let row = &bytes[4 + index * row_size..4 + (index + 1) * row_size];
        let (pid, local, remote, tcp_state) = match (protocol, family) {
            (PortProtocol::Tcp, IpFamily::V4) => {
                let state = TcpState::from_raw(u32_at(row, 0)?);
                (
                    u32_at(row, 20)?,
                    v4_endpoint(row, 4, 8)?,
                    if state == TcpState::Listen {
                        None
                    } else {
                        Some(v4_endpoint(row, 12, 16)?)
                    },
                    Some(state),
                )
            }
            (PortProtocol::Tcp, IpFamily::V6) => {
                let state = TcpState::from_raw(u32_at(row, 48)?);
                (
                    u32_at(row, 52)?,
                    v6_endpoint(row, 0, 16, 20)?,
                    if state == TcpState::Listen {
                        None
                    } else {
                        Some(v6_endpoint(row, 24, 40, 44)?)
                    },
                    Some(state),
                )
            }
            (PortProtocol::Udp, IpFamily::V4) => {
                (u32_at(row, 8)?, v4_endpoint(row, 0, 4)?, None, None)
            }
            (PortProtocol::Udp, IpFamily::V6) => {
                (u32_at(row, 24)?, v6_endpoint(row, 0, 16, 20)?, None, None)
            }
        };
        entries.push(PortEntry {
            pid,
            creation_ticks: None,
            process_name: None,
            exe: None,
            protocol,
            family,
            local,
            remote,
            tcp_state,
            identity_note: None,
        });
    }
    Ok(entries)
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset.checked_add(4).context("字段偏移溢出")?)
            .context("字段截断")?
            .try_into()?,
    ))
}
fn port_at(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(
        bytes
            .get(offset..offset.checked_add(2).context("端口偏移溢出")?)
            .context("端口截断")?
            .try_into()?,
    ))
}
fn v4_endpoint(bytes: &[u8], address: usize, port: usize) -> Result<SocketAddr> {
    let octets: [u8; 4] = bytes
        .get(address..address + 4)
        .context("IPv4 地址截断")?
        .try_into()?;
    Ok(SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::from(octets),
        port_at(bytes, port)?,
    )))
}
fn v6_endpoint(bytes: &[u8], address: usize, scope: usize, port: usize) -> Result<SocketAddr> {
    let octets: [u8; 16] = bytes
        .get(address..address + 16)
        .context("IPv6 地址截断")?
        .try_into()?;
    // Owner-PID table scope IDs are documented in network byte order.
    let scope_id = u32::from_be_bytes(
        bytes
            .get(scope..scope + 4)
            .context("IPv6 scope 截断")?
            .try_into()?,
    );
    Ok(SocketAddr::V6(SocketAddrV6::new(
        Ipv6Addr::from(octets),
        port_at(bytes, port)?,
        0,
        scope_id,
    )))
}

/// Normalized reusable search keys, prepared once per snapshot on the model.
#[derive(Clone, Debug)]
pub struct PortSearchText {
    pub name: String,
    pub local: String,
    pub remote: String,
    pub all: String,
}
impl PortSearchText {
    pub fn new(entry: &PortEntry) -> Self {
        let name = entry.process_name.as_deref().unwrap_or("").to_lowercase();
        let local = entry.local.to_string().to_lowercase();
        let remote = entry
            .remote
            .map(|value| value.to_string().to_lowercase())
            .unwrap_or_default();
        let all = format!(
            "{name}\n{local}\n{remote}\n{}\n{}\n{}\n{}",
            entry.pid,
            entry.protocol,
            entry.family,
            entry
                .tcp_state
                .map(|state| state.to_string())
                .unwrap_or_default()
        )
        .to_lowercase();
        Self {
            name,
            local,
            remote,
            all,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct PortQuery {
    tokens: Vec<String>,
}
impl PortQuery {
    pub fn new(query: &str) -> Self {
        Self {
            tokens: query
                .to_lowercase()
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        }
    }
    pub fn matches(&self, entry: &PortEntry, text: &PortSearchText) -> bool {
        self.tokens.iter().all(|token| match token.as_str() {
            "tcp" => entry.protocol == PortProtocol::Tcp,
            "udp" => entry.protocol == PortProtocol::Udp,
            "ipv4" => entry.family == IpFamily::V4,
            "ipv6" => entry.family == IpFamily::V6,
            token if token.starts_with("pid:") => token[4..].parse::<u32>().ok() == Some(entry.pid),
            token if token.starts_with("port:") => token[5..]
                .parse::<u16>()
                .ok()
                .is_some_and(|port| endpoint_port_matches(entry, port)),
            token if token.starts_with("local:") => {
                endpoint_text_matches(entry.local, &text.local, &token[6..])
            }
            token if token.starts_with("remote:") => entry
                .remote
                .is_some_and(|remote| endpoint_text_matches(remote, &text.remote, &token[7..])),
            token if token.starts_with("name:") => {
                !token[5..].is_empty() && text.name.contains(&token[5..])
            }
            token => {
                if token.bytes().all(|byte| byte.is_ascii_digit()) {
                    token.parse::<u32>().ok() == Some(entry.pid)
                        || token
                            .parse::<u16>()
                            .ok()
                            .is_some_and(|port| endpoint_port_matches(entry, port))
                } else {
                    text.all.contains(token)
                }
            }
        })
    }
}
fn endpoint_port_matches(entry: &PortEntry, port: u16) -> bool {
    entry.local.port() == port || entry.remote.is_some_and(|remote| remote.port() == port)
}
fn endpoint_text_matches(endpoint: SocketAddr, text: &str, query: &str) -> bool {
    !query.is_empty()
        && if query.bytes().all(|byte| byte.is_ascii_digit()) {
            query.parse::<u16>().ok() == Some(endpoint.port())
        } else {
            text.contains(query)
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream, UdpSocket};
    #[test]
    fn owner_tables_use_documented_layout_and_network_order_ports() {
        use std::mem::{offset_of, size_of};
        use windows::Win32::NetworkManagement::IpHelper::{
            MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
            MIB_TCPTABLE_OWNER_PID, MIB_UDP6ROW_OWNER_PID, MIB_UDP6TABLE_OWNER_PID,
            MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID,
        };
        assert_eq!(size_of::<MIB_TCPROW_OWNER_PID>(), 24);
        assert_eq!(size_of::<MIB_TCP6ROW_OWNER_PID>(), 56);
        assert_eq!(size_of::<MIB_UDPROW_OWNER_PID>(), 12);
        assert_eq!(size_of::<MIB_UDP6ROW_OWNER_PID>(), 28);
        assert_eq!(offset_of!(MIB_TCPTABLE_OWNER_PID, table), 4);
        assert_eq!(offset_of!(MIB_TCP6TABLE_OWNER_PID, table), 4);
        assert_eq!(offset_of!(MIB_UDPTABLE_OWNER_PID, table), 4);
        assert_eq!(offset_of!(MIB_UDP6TABLE_OWNER_PID, table), 4);
        let mut bytes = 1_u32.to_le_bytes().to_vec();
        let mut row = vec![0; 24];
        row[0..4].copy_from_slice(&2_u32.to_le_bytes());
        row[4..8].copy_from_slice(&[127, 0, 0, 1]);
        row[8..12].copy_from_slice(&[0x1F, 0x90, 0xAB, 0xCD]);
        row[20..24].copy_from_slice(&77_u32.to_le_bytes());
        bytes.extend(row);
        let parsed = parse_table(&bytes, PortProtocol::Tcp, IpFamily::V4).unwrap();
        assert_eq!(parsed[0].local.to_string(), "127.0.0.1:8080");
        assert_eq!(parsed[0].pid, 77);
        assert_eq!(parsed[0].remote, None);
    }
    #[test]
    fn ipv6_scope_is_network_order_and_udp_has_no_fabricated_peer_or_state() {
        let mut bytes = 1_u32.to_le_bytes().to_vec();
        let mut row = vec![0; 28];
        row[0..16].copy_from_slice(&Ipv6Addr::LOCALHOST.octets());
        row[16..20].copy_from_slice(&7_u32.to_be_bytes());
        row[20..22].copy_from_slice(&5353_u16.to_be_bytes());
        row[24..28].copy_from_slice(&9_u32.to_le_bytes());
        bytes.extend(row);
        let entry = &parse_table(&bytes, PortProtocol::Udp, IpFamily::V6).unwrap()[0];
        assert_eq!(
            entry.local,
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 5353, 0, 7))
        );
        assert_eq!(entry.remote, None);
        assert_eq!(entry.tcp_state, None);
    }
    #[test]
    fn malformed_count_length_and_win32_errors_are_rejected() {
        for bytes in [
            vec![],
            vec![0, 0, 0],
            u32::MAX.to_le_bytes().to_vec(),
            1_u32.to_le_bytes().to_vec(),
        ] {
            assert!(parse_table(&bytes, PortProtocol::Tcp, IpFamily::V6).is_err());
        }
        assert!(fetch_table(|_, _| 5).unwrap_err().to_string().contains("5"));
        assert!(
            fetch_table(|_, length| {
                *length = u32::MAX;
                ERROR_INSUFFICIENT_BUFFER.0
            })
            .is_err()
        );
        let bytes = fetch_table(|buffer, length| {
            *length = 4;
            if let Some(pointer) = buffer {
                assert_eq!(pointer as usize % 8, 0);
                unsafe {
                    pointer.cast::<u32>().write(0);
                }
                0
            } else {
                ERROR_INSUFFICIENT_BUFFER.0
            }
        })
        .unwrap();
        assert!(
            parse_table(&bytes, PortProtocol::Udp, IpFamily::V4)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn typed_queries_match_exact_pid_ports_and_protocols_without_mixing_remote_udp() {
        let mut entry = parse_table(
            &[1_u32.to_le_bytes().to_vec(), vec![0; 12]].concat(),
            PortProtocol::Udp,
            IpFamily::V4,
        )
        .unwrap()
        .remove(0);
        entry.pid = 123;
        entry.local = "127.0.0.1:8080".parse().unwrap();
        entry.process_name = Some("Owned Developer.exe".into());
        let text = PortSearchText::new(&entry);
        for query in [
            "pid:123 udp ipv4",
            "8080",
            "123",
            "port:8080 name:developer",
            "local:8080",
        ] {
            assert!(PortQuery::new(query).matches(&entry, &text), "{query}");
        }
        for query in [
            "pid:12",
            "port:80",
            "tcp",
            "remote:8080",
            "pid:invalid",
            "local:",
        ] {
            assert!(!PortQuery::new(query).matches(&entry, &text), "{query}");
        }
    }
    #[test]
    fn native_owned_ipv4_ipv6_tcp_udp_endpoints_have_exact_identity() {
        let tcp4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let tcp6 = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
        let udp4 = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let udp6 = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(tcp4.local_addr().unwrap()).unwrap();
        let (_server, _) = tcp4.accept().unwrap();
        let snapshot = PortSnapshot::collect(&[]);
        assert!(snapshot.complete, "{:?}", snapshot.issues);
        let pid = std::process::id();
        for (protocol, local) in [
            (PortProtocol::Tcp, tcp4.local_addr().unwrap()),
            (PortProtocol::Tcp, tcp6.local_addr().unwrap()),
            (PortProtocol::Udp, udp4.local_addr().unwrap()),
            (PortProtocol::Udp, udp6.local_addr().unwrap()),
        ] {
            let entry = snapshot
                .entries
                .iter()
                .find(|entry| {
                    entry.pid == pid && entry.protocol == protocol && entry.local == local
                })
                .expect("owned bound endpoint in native table");
            assert!(entry.creation_ticks.is_some(), "{:?}", entry.identity_note);
            assert!(entry.process_name.is_some());
            assert!(!entry.can_end_process(), "self is protected");
        }
        let established = snapshot
            .entries
            .iter()
            .find(|entry| {
                entry.pid == pid
                    && entry.local == client.local_addr().unwrap()
                    && entry.tcp_state == Some(TcpState::Established)
            })
            .unwrap();
        assert_eq!(established.remote, Some(tcp4.local_addr().unwrap()));
        let expected = established.creation_ticks.unwrap();
        // SAFETY: read-only handle to this owned test process, never another PID.
        let handle = OwnedProcess(
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.unwrap(),
        );
        assert_eq!(active_creation_ticks(handle.0).unwrap(), expected);
        assert!(
            read_owner(pid, expected.saturating_sub(1), None).is_err(),
            "birth after query must be rejected"
        );
        println!(
            "native owned ports: four TCP/UDP IPv4/IPv6 bindings and TCP established peer verified"
        );
    }
}
