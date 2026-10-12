//! Application groups from visible windows and verified process ancestry.
//!
//! Every input process remains visible exactly once. Independent instances are
//! never merged by basename or path. Unknown/reused parents are not connected.
//! EnumWindows covers top-level desktop-app windows on the current desktop:
//! https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-enumwindows
//! Visibility is WS_VISIBLE, including obscured/minimized windows:
//! https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-iswindowvisible
//! Foreign captions use GetWindowTextW without external SendMessage. Own-process
//! captions are skipped to avoid synchronous WM_GETTEXT on our collection worker:
//! https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-getwindowtextw

use crate::monitor::ProcessSample;
use std::collections::{HashMap, HashSet};
use windows::{
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM},
        System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
        },
    },
    core::BOOL,
};

const MAX_VISIBLE_WINDOWS: usize = 4096;
const MAX_WINDOWS_PER_PROCESS: usize = 16;
const TITLE_BUFFER_UNITS: usize = 1024;
const WINDOW_SCOPE_NOTE: &str = "应用分组依据当前桌面的可见顶层窗口；其他桌面、用户会话或托管窗口可能不可见，未确认的进程保持独立。";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub creation_time_windows_ticks: u64,
}
impl ProcessIdentity {
    pub fn from_sample(process: &ProcessSample) -> Option<Self> {
        Some(Self {
            pid: process.pid,
            creation_time_windows_ticks: process
                .start_time_windows_ticks
                .filter(|ticks| *ticks > 0)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GroupKey {
    /// Stable across ordering and sampling, including PID reuse.
    Exact(ProcessIdentity),
    /// Snapshot-local display key. Never use for mutations or remembered
    /// expansion because exact process creation identity is unavailable.
    Unverified {
        pid: u32,
        start_time_unix_secs: u64,
        input_index: usize,
    },
}
impl GroupKey {
    pub fn exact_identity(&self) -> Option<ProcessIdentity> {
        match self {
            Self::Exact(identity) => Some(*identity),
            Self::Unverified { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessGroupKind {
    Application,
    /// No positive application evidence, not a claim of OS/background status.
    Unclassified,
}

#[derive(Clone, Debug)]
pub struct VisibleWindow {
    pub owner: ProcessIdentity,
    pub title: Option<String>,
    pub title_may_be_truncated: bool,
}

#[derive(Clone, Debug, Default)]
pub struct WindowSnapshot {
    pub windows: Vec<VisibleWindow>,
    pub available: bool,
    pub unverified_process_count: usize,
    pub truncated: bool,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct GroupAggregates {
    /// Sum of whole-machine normalized member CPU, capped at 100%. Invalid
    /// values yield None. Do not divide by the logical CPU count again.
    pub cpu_usage: Option<f32>,
    pub memory_bytes: Option<u64>,
    pub working_set_bytes: Option<u64>,
    pub private_bytes: Option<u64>,
    pub private_working_set_bytes: Option<u64>,
    pub disk_read_bytes_per_sec: Option<f64>,
    pub disk_write_bytes_per_sec: Option<f64>,
    pub total_disk_read_bytes: Option<u64>,
    pub total_disk_write_bytes: Option<u64>,
    pub network_received_bytes_per_sec: Option<f64>,
    pub network_transmitted_bytes_per_sec: Option<f64>,
    /// Maximum member's busiest-engine reading, NOT the group's combined
    /// engine usage: the busiest engines of its members may differ.
    pub gpu_usage_max: Option<f32>,
    pub gpu_memory_bytes: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ProcessGroup {
    pub key: GroupKey,
    pub kind: ProcessGroupKind,
    pub root_index: usize,
    pub root_identity: Option<ProcessIdentity>,
    /// Root first, remaining members in input order. These indices refer to
    /// this exact input snapshot, never a newer sample.
    pub indices: Vec<usize>,
    pub display_name: String,
    pub primary_window_title: Option<String>,
    pub window_titles: Vec<String>,
    pub aggregates: GroupAggregates,
    pub note: Option<String>,
}

struct ProcessHandle(HANDLE);
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
fn process_creation_ticks(handle: HANDLE) -> Option<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }.ok()?;
    let ticks = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
    (ticks > 0).then_some(ticks)
}

struct WindowEnumeration {
    tracked_pids: HashSet<u32>,
    windows: Vec<(HWND, u32)>,
    per_process: HashMap<u32, usize>,
    truncated: bool,
}
unsafe extern "system" fn collect_window(hwnd: HWND, parameter: LPARAM) -> BOOL {
    // SAFETY: EnumWindows calls synchronously; this context pointer remains
    // valid for the call and is never retained or shared with other threads.
    let enumeration = unsafe { &mut *(parameter.0 as *mut WindowEnumeration) };
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }
    let mut pid = 0;
    if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) } == 0
        || !enumeration.tracked_pids.contains(&pid)
    {
        return BOOL(1);
    }
    let count = enumeration.per_process.entry(pid).or_default();
    if *count >= MAX_WINDOWS_PER_PROCESS || enumeration.windows.len() >= MAX_VISIBLE_WINDOWS {
        enumeration.truncated = true;
        return BOOL(1);
    }
    *count += 1;
    enumeration.windows.push((hwnd, pid));
    BOOL(1)
}

/// Run on a collector worker. Only visible-window PIDs are opened read-only.
/// Their exact handles remain alive while captions are read. No external
/// messages, window activation, process termination or UI mutations occur.
pub fn enumerate_visible_windows(processes: &[ProcessSample]) -> WindowSnapshot {
    let mut enumeration = WindowEnumeration {
        tracked_pids: processes.iter().map(|process| process.pid).collect(),
        windows: Vec::new(),
        per_process: HashMap::new(),
        truncated: false,
    };
    let enumeration_result = unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM((&mut enumeration as *mut WindowEnumeration) as isize),
        )
    };
    if let Err(error) = enumeration_result {
        return WindowSnapshot {
            note: Some(format!(
                "窗口枚举不可用：{error}；所有进程仍保留。{WINDOW_SCOPE_NOTE}"
            )),
            ..Default::default()
        };
    }
    let indices = unique_pid_indices(processes);
    let mut handles = HashMap::<u32, Option<(ProcessHandle, ProcessIdentity)>>::new();
    for (_, pid) in &enumeration.windows {
        handles.entry(*pid).or_insert_with(|| {
            let process = indices
                .get(pid)
                .and_then(|index| *index)
                .map(|index| &processes[index])?;
            let expected = ProcessIdentity::from_sample(process)?;
            let handle = ProcessHandle(
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, *pid) }.ok()?,
            );
            if process_creation_ticks(handle.0)? != expected.creation_time_windows_ticks {
                return None;
            }
            Some((handle, expected))
        });
    }
    let unverified_process_count = handles.values().filter(|value| value.is_none()).count();
    let own_pid = std::process::id();
    let mut windows = Vec::with_capacity(enumeration.windows.len());
    for (hwnd, pid) in enumeration.windows {
        let Some(Some((_, identity))) = handles.get(&pid) else {
            continue;
        };
        let mut current_pid = 0;
        if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut current_pid)) } == 0
            || current_pid != pid
            || !unsafe { IsWindowVisible(hwnd) }.as_bool()
        {
            continue;
        }
        let mut buffer = [0u16; TITLE_BUFFER_UNITS];
        // Own-process GetWindowTextW sends synchronous WM_GETTEXT; skip it.
        let length = if pid == own_pid {
            0
        } else {
            unsafe { GetWindowTextW(hwnd, &mut buffer) }.max(0) as usize
        };
        // HWND may be destroyed/reused between reads. Changed owners do not
        // inherit the old owner's title/application classification.
        current_pid = 0;
        if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut current_pid)) } == 0
            || current_pid != pid
            || !unsafe { IsWindowVisible(hwnd) }.as_bool()
        {
            continue;
        }
        windows.push(VisibleWindow {
            owner: *identity,
            title: (length > 0).then(|| String::from_utf16_lossy(&buffer[..length])),
            title_may_be_truncated: length == TITLE_BUFFER_UNITS - 1,
        });
    }
    let mut note = WINDOW_SCOPE_NOTE.to_owned();
    if unverified_process_count > 0 {
        note.push_str(&format!(
            " {unverified_process_count} 个窗口所属进程的精确身份不可核验，未归入应用。"
        ));
    }
    if enumeration.truncated {
        note.push_str(" 窗口数量超过安全上限；仅保留已经核验的窗口证据。");
    }
    WindowSnapshot {
        windows,
        available: true,
        unverified_process_count,
        truncated: enumeration.truncated,
        note: Some(note),
    }
}

fn unique_pid_indices(processes: &[ProcessSample]) -> HashMap<u32, Option<usize>> {
    let mut indices = HashMap::with_capacity(processes.len());
    for (index, process) in processes.iter().enumerate() {
        indices
            .entry(process.pid)
            .and_modify(|value| *value = None)
            .or_insert(Some(index));
    }
    indices
}
fn same_known_user_session(left: &ProcessSample, right: &ProcessSample) -> bool {
    matches!((left.session_id, right.session_id), (Some(left), Some(right)) if left == right)
        && matches!((&left.user_name, &right.user_name), (Some(left), Some(right))
            if !left.is_empty() && !right.is_empty() && left.eq_ignore_ascii_case(right))
}
fn same_executable(left: &ProcessSample, right: &ProcessSample) -> bool {
    matches!((&left.exe, &right.exe), (Some(left), Some(right)) if left.is_absolute() && left == right)
}

/// Break every cycle edge, iteratively in linear time.
fn remove_cycles(parents: &mut [Option<usize>]) {
    let mut state = vec![0u8; parents.len()];
    let mut path = Vec::new();
    for start in 0..parents.len() {
        if state[start] != 0 {
            continue;
        }
        path.clear();
        let mut current = Some(start);
        while let Some(index) = current {
            if state[index] != 0 {
                break;
            }
            state[index] = 1;
            path.push(index);
            current = parents[index];
        }
        if let Some(index) = current
            && state[index] == 1
        {
            let mut cycle = index;
            loop {
                let next = parents[cycle];
                parents[cycle] = None;
                if next == Some(index) {
                    break;
                }
                let Some(next) = next else {
                    break;
                };
                cycle = next;
            }
        }
        for &index in &path {
            state[index] = 2;
        }
    }
}
fn sum_u64(mut values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    values.try_fold(0u64, |sum, value| sum.checked_add(value?))
}
fn sum_rate(mut values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    values.try_fold(0.0f64, |sum, value| {
        let value = value?;
        let next = sum + value;
        (value.is_finite() && value >= 0.0 && next.is_finite()).then_some(next)
    })
}
fn aggregate(processes: &[ProcessSample], indices: &[usize]) -> GroupAggregates {
    let members = || indices.iter().map(|index| &processes[*index]);
    let cpu = members()
        .try_fold(0.0f64, |sum, process| {
            let value = process.cpu_usage;
            (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(sum + f64::from(value))
        })
        .map(|sum| sum.min(100.0) as f32);
    let gpu = members().try_fold(0.0f32, |max, process| {
        let value = process.gpu_usage?;
        (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(max.max(value))
    });
    GroupAggregates {
        cpu_usage: cpu,
        memory_bytes: sum_u64(members().map(|process| Some(process.memory_bytes))),
        working_set_bytes: sum_u64(members().map(|process| process.working_set_bytes)),
        private_bytes: sum_u64(members().map(|process| process.private_bytes)),
        private_working_set_bytes: sum_u64(
            members().map(|process| process.private_working_set_bytes),
        ),
        disk_read_bytes_per_sec: sum_rate(members().map(|process| process.disk_read_bytes_per_sec)),
        disk_write_bytes_per_sec: sum_rate(
            members().map(|process| process.disk_write_bytes_per_sec),
        ),
        total_disk_read_bytes: sum_u64(members().map(|process| process.total_disk_read_bytes)),
        total_disk_write_bytes: sum_u64(members().map(|process| process.total_disk_write_bytes)),
        network_received_bytes_per_sec: sum_rate(
            members().map(|process| process.network_received_bytes_per_sec),
        ),
        network_transmitted_bytes_per_sec: sum_rate(
            members().map(|process| process.network_transmitted_bytes_per_sec),
        ),
        gpu_usage_max: gpu,
        gpu_memory_bytes: sum_u64(members().map(|process| process.gpu_memory_bytes)),
    }
}

/// Linear in process count plus bounded window metadata. Cached windows require
/// exact PID/FILETIME matches in this sample. Never run from the render path.
///
/// Windowed descendants with a different executable become independent apps.
/// Same-executable windowed descendants retain their ancestor application.
/// Hidden descendants join only via exact non-reused ancestry and matching
/// known user/session. No-window branches without an application stay raw.
pub fn build_process_groups(
    processes: &[ProcessSample],
    windows: &WindowSnapshot,
) -> Vec<ProcessGroup> {
    let count = processes.len();
    let pid_indices = unique_pid_indices(processes);
    let identities: Vec<_> = processes
        .iter()
        .map(|process| {
            if pid_indices.get(&process.pid).copied().flatten().is_some() {
                ProcessIdentity::from_sample(process)
            } else {
                None
            }
        })
        .collect();
    let mut window_by_index = vec![Vec::new(); count];
    if windows.available {
        for window in &windows.windows {
            let Some(Some(index)) = pid_indices.get(&window.owner.pid) else {
                continue;
            };
            if identities[*index] == Some(window.owner) {
                window_by_index[*index].push(window);
            }
        }
    }
    let mut parents: Vec<_> = processes
        .iter()
        .enumerate()
        .map(|(index, process)| {
            let parent_index = pid_indices.get(&process.parent_pid?).copied().flatten()?;
            let parent = &processes[parent_index];
            let parent_identity = identities[parent_index]?;
            let child_identity = identities[index]?;
            (parent_index != index
                && parent_identity.creation_time_windows_ticks
                    <= child_identity.creation_time_windows_ticks
                && same_known_user_session(parent, process))
            .then_some(parent_index)
        })
        .collect();
    remove_cycles(&mut parents);
    let mut resolved = vec![false; count];
    let mut anchors = vec![None; count];
    let mut path = Vec::new();
    for start in 0..count {
        if resolved[start] {
            continue;
        }
        path.clear();
        let mut current = Some(start);
        while let Some(index) = current {
            if resolved[index] {
                break;
            }
            path.push(index);
            current = parents[index];
        }
        for &index in path.iter().rev() {
            let inherited = parents[index].and_then(|parent| anchors[parent]);
            anchors[index] = if !window_by_index[index].is_empty() {
                match inherited {
                    Some(root) if same_executable(&processes[root], &processes[index]) => {
                        Some(root)
                    }
                    _ => Some(index),
                }
            } else {
                inherited
            };
            resolved[index] = true;
        }
    }
    let mut groups = Vec::<ProcessGroup>::new();
    let mut root_groups = vec![None; count];
    for index in 0..count {
        let root = anchors[index].unwrap_or(index);
        let group_index = match root_groups[root] {
            Some(group_index) => group_index,
            None => {
                let process = &processes[root];
                let identity = identities[root];
                let kind = if anchors[root].is_some() {
                    ProcessGroupKind::Application
                } else {
                    ProcessGroupKind::Unclassified
                };
                let key = identity
                    .map(GroupKey::Exact)
                    .unwrap_or(GroupKey::Unverified {
                        pid: process.pid,
                        start_time_unix_secs: process.start_time_unix_secs,
                        input_index: root,
                    });
                let primary_window_title = window_by_index[root]
                    .iter()
                    .find_map(|window| window.title.clone());
                let display_name = if process.name.is_empty() {
                    format!("PID {}", process.pid)
                } else {
                    process.name.clone()
                };
                let group_index = groups.len();
                groups.push(ProcessGroup {
                    key, kind, root_index: root, root_identity: identity, indices: Vec::new(),
                    display_name, primary_window_title, window_titles: Vec::new(),
                    aggregates: GroupAggregates::default(),
                    note: if kind == ProcessGroupKind::Application {
                        Some("成员按已核验父子关系分组；GPU 显示成员最忙占用的最大值，不代表整组引擎总占用。".into())
                    } else { windows.note.clone() },
                });
                root_groups[root] = Some(group_index);
                group_index
            }
        };
        groups[group_index].indices.push(index);
    }
    for group in &mut groups {
        if let Some(position) = group
            .indices
            .iter()
            .position(|index| *index == group.root_index)
        {
            // Move root first without reordering remaining children.
            group.indices[..=position].rotate_right(1);
        }
        let mut seen = HashSet::new();
        for &index in &group.indices {
            for window in &window_by_index[index] {
                if let Some(title) = &window.title
                    && seen.insert(title.as_str())
                {
                    group.window_titles.push(title.clone());
                }
            }
        }
        group.aggregates = aggregate(processes, &group.indices);
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, time::Instant};

    fn process(pid: u32, parent: Option<u32>, ticks: u64, exe: &str) -> ProcessSample {
        ProcessSample {
            pid,
            parent_pid: parent,
            name: format!("app-{pid}.exe"),
            start_time_windows_ticks: Some(ticks),
            session_id: Some(1),
            user_name: Some("WORKSTATION\\Alice".into()),
            exe: Some(PathBuf::from(exe)),
            cpu_usage: 1.0,
            memory_bytes: 100,
            ..Default::default()
        }
    }
    fn window_snapshot(processes: &[ProcessSample], indices: &[usize]) -> WindowSnapshot {
        WindowSnapshot {
            available: true,
            windows: indices
                .iter()
                .map(|index| VisibleWindow {
                    owner: ProcessIdentity::from_sample(&processes[*index]).unwrap(),
                    title: Some(format!("window-{}", processes[*index].pid)),
                    title_may_be_truncated: false,
                })
                .collect(),
            ..Default::default()
        }
    }
    fn assert_partition(groups: &[ProcessGroup], count: usize) {
        let mut seen = vec![0; count];
        for group in groups {
            assert_eq!(group.indices[0], group.root_index);
            for &index in &group.indices {
                seen[index] += 1;
            }
        }
        assert!(seen.iter().all(|seen| *seen == 1));
    }

    #[test]
    fn descendants_group_without_absorbing_other_windowed_apps() {
        let processes = vec![
            process(1, None, 10, r"C:\Apps\Browser.exe"),
            process(2, Some(1), 20, r"C:\Apps\Browser.exe"),
            process(3, Some(2), 30, r"C:\Apps\renderer.exe"),
            process(4, Some(1), 40, r"C:\Other\Editor.exe"),
            process(5, Some(4), 50, r"C:\Other\worker.exe"),
            process(6, None, 60, r"C:\Other\daemon.exe"),
        ];
        let groups = build_process_groups(&processes, &window_snapshot(&processes, &[0, 1, 3]));
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].indices, vec![0, 1, 2]);
        assert_eq!(groups[1].indices, vec![3, 4]);
        assert_eq!(groups[2].kind, ProcessGroupKind::Unclassified);
        assert_eq!(groups[0].aggregates.memory_bytes, Some(300));
        assert_eq!(groups[0].aggregates.cpu_usage, Some(3.0));
        assert_partition(&groups, processes.len());
    }

    #[test]
    fn cached_windows_and_parent_pids_never_cross_creation_identities() {
        let mut processes = vec![
            process(1, None, 90, r"C:\App.exe"),
            process(2, Some(1), 50, r"C:\worker.exe"),
        ];
        let windows = window_snapshot(&processes, &[0]);
        assert_eq!(build_process_groups(&processes, &windows).len(), 2);
        processes[0].start_time_windows_ticks = Some(100);
        let groups = build_process_groups(&processes, &windows);
        assert!(
            groups
                .iter()
                .all(|group| group.kind == ProcessGroupKind::Unclassified)
        );
        assert_ne!(groups[0].key, GroupKey::Exact(windows.windows[0].owner));
        processes[0].start_time_windows_ticks = None;
        let groups = build_process_groups(&processes, &windows);
        assert!(groups[0].root_identity.is_none());
        assert!(groups[0].key.exact_identity().is_none());
        assert_partition(&groups, processes.len());
    }

    #[test]
    fn different_paths_users_sessions_and_unknown_scope_do_not_merge() {
        let mut processes = vec![
            process(1, None, 10, r"C:\One\Same.exe"),
            process(2, None, 20, r"C:\Two\Same.exe"),
            process(3, Some(1), 30, r"C:\One\Same.exe"),
            process(4, Some(1), 40, r"C:\One\Same.exe"),
            process(5, Some(1), 50, r"C:\One\Same.exe"),
            process(6, None, 60, r"C:\One\Same.exe"),
        ];
        processes[2].user_name = Some("WORKSTATION\\Bob".into());
        processes[3].session_id = Some(2);
        processes[4].user_name = None;
        let groups = build_process_groups(&processes, &window_snapshot(&processes, &[0, 1, 5]));
        assert_eq!(groups.len(), processes.len());
        assert_partition(&groups, processes.len());
    }

    #[test]
    fn unclassified_parent_does_not_take_descendant_application() {
        let processes = vec![
            process(1, None, 10, r"C:\Windows\explorer.exe"),
            process(2, Some(1), 20, r"C:\App.exe"),
            process(3, Some(2), 30, r"C:\worker.exe"),
        ];
        let groups = build_process_groups(&processes, &window_snapshot(&processes, &[1]));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].kind, ProcessGroupKind::Unclassified);
        assert_eq!(groups[1].indices, vec![1, 2]);
    }

    #[test]
    fn optional_aggregates_preserve_missing_and_gpu_is_maximum() {
        let mut processes = vec![
            process(1, None, 10, r"C:\App.exe"),
            process(2, Some(1), 20, r"C:\App.exe"),
        ];
        processes[0].cpu_usage = 70.0;
        processes[1].cpu_usage = 50.0;
        for (index, process) in processes.iter_mut().enumerate() {
            process.gpu_usage = Some(if index == 0 { 12.0 } else { 35.0 });
            process.gpu_memory_bytes = Some(25);
            process.disk_read_bytes_per_sec = Some(5.0);
            process.network_received_bytes_per_sec = Some(0.0);
        }
        let windows = window_snapshot(&processes, &[0]);
        let groups = build_process_groups(&processes, &windows);
        let values = &groups[0].aggregates;
        assert_eq!(values.cpu_usage, Some(100.0));
        assert_eq!(values.gpu_usage_max, Some(35.0));
        assert_eq!(values.gpu_memory_bytes, Some(50));
        assert_eq!(values.disk_read_bytes_per_sec, Some(10.0));
        assert_eq!(values.network_received_bytes_per_sec, Some(0.0));
        assert_eq!(values.network_transmitted_bytes_per_sec, None);
        assert_eq!(values.private_bytes, None);
        processes[1].disk_read_bytes_per_sec = None;
        processes[1].gpu_usage = None;
        processes[1].network_received_bytes_per_sec = None;
        let groups = build_process_groups(&processes, &windows);
        assert_eq!(groups[0].aggregates.gpu_usage_max, None);
        assert_eq!(groups[0].aggregates.disk_read_bytes_per_sec, None);
        assert_eq!(groups[0].aggregates.network_received_bytes_per_sec, None);
    }

    #[test]
    fn overflow_nonfinite_and_negative_values_are_not_fabricated() {
        let mut processes = vec![
            process(1, None, 10, r"C:\App.exe"),
            process(2, Some(1), 20, r"C:\App.exe"),
        ];
        processes[0].memory_bytes = u64::MAX;
        processes[0].cpu_usage = f32::NAN;
        processes[0].disk_read_bytes_per_sec = Some(f64::INFINITY);
        processes[1].disk_read_bytes_per_sec = Some(0.0);
        processes[0].network_received_bytes_per_sec = Some(-1.0);
        processes[1].network_received_bytes_per_sec = Some(0.0);
        let groups = build_process_groups(&processes, &window_snapshot(&processes, &[0]));
        let values = &groups[0].aggregates;
        assert!(values.memory_bytes.is_none());
        assert!(values.cpu_usage.is_none());
        assert!(values.disk_read_bytes_per_sec.is_none());
        assert!(values.network_received_bytes_per_sec.is_none());
    }

    #[test]
    fn enumeration_failure_keeps_all_raw_rows() {
        let processes = vec![
            process(1, None, 10, r"C:\App.exe"),
            process(2, Some(1), 20, r"C:\App.exe"),
        ];
        let mut failed = window_snapshot(&processes, &[0]);
        failed.available = false;
        failed.note = Some("cannot enumerate desktop".into());
        let groups = build_process_groups(&processes, &failed);
        assert_eq!(groups.len(), processes.len());
        assert!(
            groups
                .iter()
                .all(|group| group.kind == ProcessGroupKind::Unclassified)
        );
        assert_eq!(groups[0].note.as_deref(), Some("cannot enumerate desktop"));
        assert_partition(&groups, processes.len());
    }

    #[test]
    fn duplicate_pids_and_cycles_do_not_invent_ancestry() {
        let processes = vec![
            process(1, Some(2), 10, r"C:\App.exe"),
            process(2, Some(1), 10, r"C:\App.exe"),
            process(3, Some(3), 10, r"C:\App.exe"),
            process(4, Some(1), 20, r"C:\worker.exe"),
            process(5, None, 30, r"C:\App.exe"),
            process(5, None, 31, r"C:\App.exe"),
        ];
        let groups = build_process_groups(&processes, &window_snapshot(&processes, &[0, 1, 2, 4]));
        assert_eq!(groups[0].indices, vec![0, 3]);
        assert_eq!(groups[1].indices, vec![1]);
        assert_eq!(groups[2].indices, vec![2]);
        assert!(
            groups
                .iter()
                .filter(|group| group.root_index >= 4)
                .all(|group| group.kind == ProcessGroupKind::Unclassified
                    && group.root_identity.is_none())
        );
        assert_partition(&groups, processes.len());
    }

    #[test]
    fn exact_keys_survive_reordering_and_children_keep_raw_order() {
        let processes = vec![
            process(1, None, 10, r"C:\App.exe"),
            process(2, Some(1), 20, r"C:\App.exe"),
            process(3, Some(1), 30, r"C:\App.exe"),
        ];
        let windows = window_snapshot(&processes, &[0]);
        let original = build_process_groups(&processes, &windows);
        let reordered_input = vec![
            processes[2].clone(),
            processes[1].clone(),
            processes[0].clone(),
        ];
        let reordered = build_process_groups(&reordered_input, &windows);
        assert_eq!(original[0].key, reordered[0].key);
        assert_eq!(reordered[0].indices, vec![2, 0, 1]);
        assert_partition(&reordered, reordered_input.len());
    }

    #[test]
    fn ten_thousand_deep_wide_and_cyclic_nodes_are_iterative_linear_work() {
        for wide in [false, true] {
            let processes: Vec<_> = (1..=10_000u32)
                .map(|pid| {
                    process(
                        pid,
                        (pid > 1).then_some(if wide { 1 } else { pid - 1 }),
                        u64::from(pid),
                        r"C:\App.exe",
                    )
                })
                .collect();
            let windows = window_snapshot(&processes, &[0]);
            let started = Instant::now();
            let groups = build_process_groups(&processes, &windows);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].indices.len(), 10_000);
            assert_eq!(groups[0].aggregates.memory_bytes, Some(1_000_000));
            assert_partition(&groups, processes.len());
            eprintln!(
                "process_groups: shape={} count=10000 elapsed_ms={:.3}",
                if wide { "wide" } else { "deep" },
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
        let processes: Vec<_> = (1..=10_000u32)
            .map(|pid| {
                process(
                    pid,
                    Some(if pid == 10_000 { 1 } else { pid + 1 }),
                    1,
                    r"C:\App.exe",
                )
            })
            .collect();
        let groups = build_process_groups(&processes, &window_snapshot(&processes, &[0]));
        assert_eq!(groups.len(), 10_000);
        assert_partition(&groups, processes.len());
    }

    #[test]
    #[ignore = "Creates only an owned temporary GUI process; requires an interactive Windows desktop"]
    fn live_owned_gui_window_is_visible_and_exact_identity_is_required() {
        use std::{
            os::windows::{io::AsRawHandle, process::CommandExt},
            process::{Child, Command, Stdio},
            time::{Duration, SystemTime, UNIX_EPOCH},
        };
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        struct OwnedChild(Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                if !matches!(self.0.try_wait(), Ok(Some(_))) && self.0.kill().is_err() {
                    return;
                }
                let _ = self.0.wait();
            }
        }
        let title = format!(
            "SystemMonitorX-owned-grouping-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; $f = [System.Windows.Forms.Form]::new(); $f.Text = '{title}'; $f.Width = 280; $f.Height = 90; $f.Show(); $end = [DateTime]::UtcNow.AddSeconds(20); while ([DateTime]::UtcNow -lt $end) {{ [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 20 }}; $f.Dispose()"
        );
        let mut child = OwnedChild(
            Command::new(powershell)
                .args(["-NoProfile", "-NonInteractive", "-Sta", "-Command", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW.0)
                .spawn()
                .unwrap(),
        );
        let ticks = process_creation_ticks(HANDLE(child.0.as_raw_handle())).unwrap();
        let processes = vec![process(child.0.id(), None, ticks, r"C:\OwnedFixture.exe")];
        let started = Instant::now();
        let snapshot = loop {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "owned fixture exited prematurely"
            );
            let snapshot = enumerate_visible_windows(&processes);
            if snapshot
                .windows
                .iter()
                .any(|window| window.title.as_deref() == Some(title.as_str()))
            {
                break snapshot;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "owned GUI wasn't enumerated: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        let groups = build_process_groups(&processes, &snapshot);
        assert_eq!(groups[0].kind, ProcessGroupKind::Application);
        assert_eq!(
            groups[0].primary_window_title.as_deref(),
            Some(title.as_str())
        );
        let wrong_identity = vec![process(
            child.0.id(),
            None,
            ticks + 1,
            r"C:\OwnedFixture.exe",
        )];
        let rejected = enumerate_visible_windows(&wrong_identity);
        assert!(rejected.windows.is_empty());
        assert_eq!(rejected.unverified_process_count, 1);
        eprintln!(
            "owned GUI visible: pid={} creation_ticks={} title_verified=true stale_identity_rejected=true",
            child.0.id(),
            ticks
        );
    }
}
