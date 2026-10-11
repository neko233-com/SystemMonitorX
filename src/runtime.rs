use anyhow::{Context, Result, ensure};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND},
        UI::{
            Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::{
                GetWindowThreadProcessId, HWND_NOTOPMOST, HWND_TOPMOST, SW_SHOWNORMAL,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
            },
        },
    },
    core::{PCWSTR, w},
};

/// Applies only to a window belonging to this process.
pub fn set_window_topmost(raw_hwnd: isize, enabled: bool) -> Result<()> {
    let hwnd = HWND(raw_hwnd as *mut core::ffi::c_void);
    let mut owner = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
    ensure!(owner == std::process::id(), "不能修改其他应用的窗口");
    unsafe {
        SetWindowPos(
            hwnd,
            Some(if enabled {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            }),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    }
    .context("无法设置窗口置顶")
}

/// Called from a background task only after the user presses the administrator button.
/// Windows owns the UAC consent prompt. The current app quits only on success.
pub fn relaunch_elevated() -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let executable = std::env::current_exe()
        .context("找不到当前程序路径")?
        .canonicalize()?;
    let wide: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(wide.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }
        .context("管理员启动未完成：可能取消了 Windows 权限确认")?;
    if !info.hProcess.is_invalid() {
        let _ = unsafe { CloseHandle(info.hProcess) };
    }
    Ok(())
}
