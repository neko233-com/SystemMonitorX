//! Explicitly opted-in native update installation smoke.
//! Success here records a real UI handoff request; the external driver must
//! verify the helper's install result, registry and restarted executable.
use super::{Page, SystemMonitorApp};
use anyhow::{Context as _, Result, anyhow, bail, ensure};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, App, Entity, ScrollDelta, point, px, size};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use system_monitor_x::updater::{UpdatePhase, verify_file};

fn write_report(directory: &Path, report: &serde_json::Value) -> Result<()> {
    std::fs::create_dir_all(directory)?;
    std::fs::write(
        directory.join("ui-update-report.json"),
        serde_json::to_vec_pretty(report)?,
    )?;
    Ok(())
}

fn click_control(
    handle: AnyWindowHandle,
    id: &'static str,
    scroll: bool,
    cx: &mut App,
) -> Result<()> {
    handle.update(cx, |_, window, cx| {
        for _ in 0..10 {
            window.render_frame(cx);
            if window.try_find(id).is_some_and(|target| target.visible()) {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| window.click(id, cx)))
                    .map_err(|_| anyhow!("native click failed: {id}"))?;
                return Ok(());
            }
            if !scroll {
                break;
            }
            ensure!(
                window
                    .try_find("settings-scroll")
                    .is_some_and(|target| target.visible()),
                "settings scroll surface is missing"
            );
            window.scroll(
                "settings-scroll",
                ScrollDelta::Pixels(point(px(0.), px(-160.))),
                cx,
            );
        }
        bail!("native update control is unavailable: {id}")
    })?
}

pub fn start(
    handle: AnyWindowHandle,
    view: Entity<SystemMonitorApp>,
    expected: String,
    report_directory: PathBuf,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        let mut report = serde_json::json!({
            "passed": false,
            "phase": "starting",
            "expectedVersion": expected,
            "handOffRequested": false,
            "installationVerified": false,
            "pid": std::process::id(),
        });
        let result: Result<()> = async {
            let requested = expected.strip_prefix('v').unwrap_or(&expected);
            let version = semver::Version::parse(requested).context("invalid expected version")?;
            ensure!(version.pre.is_empty() && version.build.is_empty(), "expected version must be stable");
            let expected = version.to_string();
            write_report(&report_directory, &report)?;

            let telemetry_deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let ready = cx.update(|cx| {
                    let app = view.read(cx);
                    app.snapshot.as_ref().is_some_and(|snapshot| {
                        snapshot.cpu_ready && !snapshot.processes.is_empty()
                    })
                });
                if ready { break; }
                ensure!(Instant::now() < telemetry_deadline, "real telemetry timed out");
                smol::Timer::after(Duration::from_millis(100)).await;
            }

            cx.update(|cx| -> Result<()> {
                handle.update(cx, |_, window, _| { window.resize(size(px(1380.), px(1000.)));
                })?;
                click_control(handle, "nav-设置", false, cx)?;
                ensure!(view.read(cx).page == Page::Settings, "settings navigation failed");
                Ok(())
            })?;

            let check_deadline = Instant::now() + Duration::from_secs(120);
            loop {
                let busy = cx.update(|cx| matches!(
                    view.read(cx).update_state.phase,
                    UpdatePhase::Checking | UpdatePhase::Downloading { .. }
                ));
                if !busy { break; }
                ensure!(Instant::now() < check_deadline, "previous update operation timed out");
                smol::Timer::after(Duration::from_millis(100)).await;
            }
            let before_check = cx.update(|cx| view.read(cx).updater.snapshot().sequence);
            cx.update(|cx| click_control(handle, "check-update", true, cx))?;
            report["phase"] = "checking".into();
            write_report(&report_directory, &report)?;

            let release = loop {
                let state = cx.update(|cx| view.read(cx).updater.snapshot());
                if state.sequence != before_check {
                    match state.phase {
                        UpdatePhase::Failed(ref error) => bail!("real update check failed: {error}"),
                        UpdatePhase::Current | UpdatePhase::Available => {
                            let release = state.release.context("real check has no release")?;
                            ensure!(release.version == expected,
                                "public stable version is {}, expected {}", release.version, expected);
                            break release;
                        }
                        _ => {}
                    }
                }
                ensure!(Instant::now() < check_deadline, "public release check timed out");
                smol::Timer::after(Duration::from_millis(100)).await;
            };
            report["releaseVersion"] = release.version.clone().into();
            report["releasePage"] = release.page_url.clone().into();
            report["installerUrl"] = release.installer_url.clone().into();
            report["sha256"] = release.installer_sha256.clone().into();
            report["size"] = release.installer_bytes.into();
            report["phase"] = "downloading".into();
            write_report(&report_directory, &report)?;

            // This explicit installation smoke is allowed to reinstall the
            // same stable version. It invokes the real worker without fabricating
            // UI state or changing the application's version.
            cx.update(|cx| view.read(cx).updater.download());
            let download_deadline = Instant::now() + Duration::from_secs(180);
            let package = loop {
                let state = cx.update(|cx| view.read(cx).updater.snapshot());
                match state.phase {
                    UpdatePhase::Failed(ref error) => bail!("real download failed: {error}"),
                    UpdatePhase::Ready(ref path) => {
                        ensure!(state.release.as_ref() == Some(&release), "release changed while downloading");
                        let displayed_ready = cx.update(|cx| {
                            let app = view.read(cx);
                            app.update_state.release.as_ref() == Some(&release)
                                && matches!(&app.update_state.phase, UpdatePhase::Ready(displayed) if displayed == path)
                                && !app.operation_busy
                        });
                        if displayed_ready { break path.clone(); }
                    }
                    _ => {}
                }
                ensure!(Instant::now() < download_deadline, "verified download or Ready UI timed out");
                smol::Timer::after(Duration::from_millis(100)).await;
            };

            let verified_package = package.clone();
            let verified_release = release.clone();
            let actual_size = smol::unblock(move || -> Result<u64> {
                verify_file(&verified_package, &verified_release)?;
                Ok(std::fs::metadata(&verified_package)?.len())
            }).await?;
            ensure!(actual_size == release.installer_bytes, "verified package size changed");
            report["path"] = package.to_string_lossy().into_owned().into();
            report["size"] = actual_size.into();
            report["phase"] = "ready".into();
            report["sha256Verified"] = true.into();
            write_report(&report_directory, &report)?;

            // Persist after native dispatch in the same update callback, before
            // yielding. Only install_update owns helper launch and success quit.
            cx.update(|cx| -> Result<()> {
                click_control(handle, "install-update", true, cx)?;
                ensure!(view.read(cx).operation_busy, "Install click did not request the normal handoff");
                report["handOffRequested"] = true.into();
                report["passed"] = true.into();
                report["phase"] = "handoff_requested".into();
                write_report(&report_directory, &report)?;
                Ok(())
            })?;

            let handoff_deadline = Instant::now() + Duration::from_secs(60);
            loop {
                smol::Timer::after(Duration::from_millis(100)).await;
                let error = cx.update(|cx| {
                    let app = view.read(cx);
                    (!app.operation_busy).then(|| app.message.clone()).flatten()
                        .filter(|message| message.starts_with("启动安装器失败"))
                });
                if let Some(error) = error { bail!("{error}"); }
                ensure!(Instant::now() < handoff_deadline, "normal installer handoff did not quit the application");
                // On success the normal install_update callback quits the app,
                // dropping this task. Do not launch helpers or terminate here.
            }
        }.await.context("native update installation smoke");

        if let Err(error) = result {
            report["passed"] = false.into();
            report["phase"] = "failed".into();
            report["error"] = format!("{error:#}").into();
            if let Err(write_error) = write_report(&report_directory, &report) {
                eprintln!("Update smoke report failed: {write_error:#}; original error: {error:#}");
            }
            cx.update(|cx| cx.quit());
            std::process::exit(1);
        }
    }).detach();
}
