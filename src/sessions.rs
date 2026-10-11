//! Read-only Windows Terminal Services session discovery and live resource totals.
//! Session identity includes the native logon timestamp, not just a reusable ID.

use anyhow::{Context, Result, ensure};
use windows::Win32::System::RemoteDesktop::{
    WTS_CONNECTSTATE_CLASS, WTS_INFO_CLASS, WTS_SESSION_INFOW, WTSActive, WTSClientName,
    WTSClientProtocolType, WTSConnected, WTSDisconnected, WTSDomainName, WTSDown,
    WTSEnumerateSessionsW, WTSFreeMemory, WTSINFOW, WTSIdle, WTSInit, WTSListen,
    WTSQuerySessionInformationW, WTSReset, WTSSessionInfo, WTSUserName,
};
use windows::core::PWSTR;

use crate::monitor::ProcessSample;

#[derive(Clone, Debug, Default)]
pub struct SessionSample {
    pub id: u32,
    pub station_name: String,
    pub state: String,
    pub user_name: Option<String>,
    pub domain: Option<String>,
    pub client_name: Option<String>,
    pub protocol: Option<String>,
    /// WTSINFOW.LogonTime, in Windows FILETIME ticks, used for action verification.
    pub logon_time_windows_ticks: Option<i64>,
    pub process_count: usize,
    pub cpu_usage: f32,
    pub memory_bytes: u64,
    /// Busiest measured process in this session; this is not an engine sum.
    pub gpu_usage: Option<f32>,
}

struct WtsAllocation(*mut std::ffi::c_void);

impl Drop for WtsAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: each allocation is an owned successful WTS API result.
            unsafe { WTSFreeMemory(self.0) };
        }
    }
}

pub fn enumerate_sessions() -> Result<Vec<SessionSample>> {
    let mut pointer = std::ptr::null_mut::<WTS_SESSION_INFOW>();
    let mut count = 0;
    // SAFETY: both output locations are valid, current-server handle is None,
    // and the reserved/version arguments follow the WTS enumeration contract.
    unsafe { WTSEnumerateSessionsW(None, 0, 1, &mut pointer, &mut count) }
        .context("无法读取 Windows 用户会话")?;
    let _allocation = WtsAllocation(pointer.cast());
    ensure!(count <= 65_536, "Windows 会话列表长度无效");
    if count == 0 {
        return Ok(Vec::new());
    }
    ensure!(!pointer.is_null(), "Windows 未提供会话列表缓冲区");
    // SAFETY: WTS returned `count` initialized entries in the owned allocation.
    let entries = unsafe { std::slice::from_raw_parts(pointer, count as usize) };
    let mut sessions = Vec::with_capacity(entries.len());
    for entry in entries {
        let station_name = if entry.pWinStationName.is_null() {
            String::new()
        } else {
            // SAFETY: each station name is a WTS-owned null-terminated string,
            // and the enumeration allocation remains alive throughout this loop.
            unsafe { entry.pWinStationName.to_string() }.unwrap_or_default()
        };
        let logon_time =
            query_buffer(entry.SessionId, WTSSessionInfo).and_then(|(allocation, bytes)| {
                if bytes < std::mem::size_of::<WTSINFOW>() {
                    return None;
                }
                // SAFETY: the query class returned at least one complete WTSINFOW.
                let info = unsafe { std::ptr::read_unaligned(allocation.0.cast::<WTSINFOW>()) };
                (info.LogonTime > 0).then_some(info.LogonTime)
            });
        let protocol =
            query_buffer(entry.SessionId, WTSClientProtocolType).and_then(|(allocation, bytes)| {
                if bytes < 2 {
                    return None;
                }
                // SAFETY: this WTS info class is a two-byte unsigned protocol ID.
                let value = unsafe { std::ptr::read_unaligned(allocation.0.cast::<u16>()) };
                Some(match value {
                    0 => "本地控制台".into(),
                    2 => "RDP".into(),
                    _ => format!("协议 {value}"),
                })
            });
        sessions.push(SessionSample {
            id: entry.SessionId,
            station_name,
            state: session_state(entry.State).to_owned(),
            user_name: query_string(entry.SessionId, WTSUserName),
            domain: query_string(entry.SessionId, WTSDomainName),
            client_name: query_string(entry.SessionId, WTSClientName),
            protocol,
            logon_time_windows_ticks: logon_time,
            ..Default::default()
        });
    }
    sessions.sort_by_key(|session| session.id);
    Ok(sessions)
}

fn query_buffer(id: u32, kind: WTS_INFO_CLASS) -> Option<(WtsAllocation, usize)> {
    let mut buffer = PWSTR::null();
    let mut bytes = 0;
    // SAFETY: output arguments are valid; the owned allocation is freed by RAII.
    unsafe { WTSQuerySessionInformationW(None, id, kind, &mut buffer, &mut bytes) }.ok()?;
    let allocation = WtsAllocation(buffer.0.cast());
    if buffer.is_null() || bytes > 1024 * 1024 {
        return None;
    }
    Some((allocation, bytes as usize))
}

fn query_string(id: u32, kind: WTS_INFO_CLASS) -> Option<String> {
    let (allocation, bytes) = query_buffer(id, kind)?;
    if bytes < 2 || bytes % 2 != 0 {
        return None;
    }
    // SAFETY: WTS string queries return aligned UTF-16 within this byte count.
    let units = unsafe { std::slice::from_raw_parts(allocation.0.cast::<u16>(), bytes / 2) };
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    let text = String::from_utf16_lossy(&units[..end]);
    (!text.is_empty()).then_some(text)
}

#[allow(non_upper_case_globals)] // These are the names exported by Windows metadata.
fn session_state(state: WTS_CONNECTSTATE_CLASS) -> &'static str {
    match state {
        WTSActive => "活动",
        WTSConnected => "已连接",
        WTSDisconnected => "已断开",
        WTSIdle => "空闲",
        WTSListen => "监听",
        WTSReset => "重置中",
        WTSDown => "已关闭",
        WTSInit => "初始化中",
        _ => "连接中",
    }
}

/// Static WTS metadata can be refreshed slowly; live process totals must be new.
pub fn summarize_sessions(sessions: &mut [SessionSample], processes: &[ProcessSample]) {
    for session in sessions.iter_mut() {
        session.process_count = 0;
        session.cpu_usage = 0.0;
        session.memory_bytes = 0;
        session.gpu_usage = None;
    }
    for process in processes {
        if let Some(session) = sessions
            .iter_mut()
            .find(|session| Some(session.id) == process.session_id)
        {
            session.process_count += 1;
            session.cpu_usage = (session.cpu_usage + process.cpu_usage).clamp(0.0, 100.0);
            session.memory_bytes = session.memory_bytes.saturating_add(process.memory_bytes);
            if let Some(gpu) = process.gpu_usage.filter(|value| value.is_finite()) {
                session.gpu_usage =
                    Some(session.gpu_usage.map_or(gpu, |previous| previous.max(gpu)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_totals_use_session_ids_and_keep_missing_gpu_unknown() {
        let mut sessions = vec![
            SessionSample {
                id: 1,
                ..Default::default()
            },
            SessionSample {
                id: 2,
                ..Default::default()
            },
        ];
        let processes = vec![
            ProcessSample {
                session_id: Some(1),
                cpu_usage: 12.5,
                memory_bytes: 100,
                gpu_usage: Some(10.0),
                ..Default::default()
            },
            ProcessSample {
                session_id: Some(1),
                cpu_usage: 2.5,
                memory_bytes: 200,
                gpu_usage: Some(20.0),
                ..Default::default()
            },
            ProcessSample {
                session_id: None,
                memory_bytes: 999,
                ..Default::default()
            },
        ];
        summarize_sessions(&mut sessions, &processes);
        assert_eq!(sessions[0].process_count, 2);
        assert_eq!(sessions[0].memory_bytes, 300);
        assert_eq!(sessions[0].cpu_usage, 15.0);
        assert_eq!(sessions[0].gpu_usage, Some(20.0));
        assert_eq!(sessions[1].gpu_usage, None);
    }
}
