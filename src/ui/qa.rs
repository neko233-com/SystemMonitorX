//! Opt-in tests use the real Windows window, D3D11 renderer and native GPUI
//! event dispatch. No mock telemetry or simulated browser UI is involved.
use super::*;
use anyhow::{Context as _, Result, ensure};
use gpui_kit::test::TestWindowExt;

fn capture(handle: AnyWindowHandle, cx: &mut App, path: &std::path::Path) -> Result<()> {
    handle.update(cx, |_, window, cx| {
        window.render_frame(cx);
        window.render_to_image()?.save(path)?;
        Ok::<(), anyhow::Error>(())
    })?
}

pub fn start(
    handle: AnyWindowHandle,
    view: Entity<SystemMonitorApp>,
    directory: PathBuf,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        let mut performance = serde_json::Value::Null;
        let mut icon_source = serde_json::Value::Null;
        let result: Result<()> = async {
            std::fs::create_dir_all(&directory)?;
            let owned_tcp = std::net::TcpListener::bind("127.0.0.1:0")?;
            let owned_udp = std::net::UdpSocket::bind("127.0.0.1:0")?;
            smol::Timer::after(Duration::from_secs(6)).await;
            cx.update(|cx| -> Result<()> {
                ensure!(view.read(cx).snapshot.as_ref().is_some_and(|s|s.cpu_ready), "real telemetry did not arrive");
                ensure!(!view.read(cx).processes.read(cx).delegate().rows.is_empty(), "no processes");
                capture(handle,cx,&directory.join("01-processes.png"))?;
                handle.update(cx,|_,w,cx| {w.press("ctrl-f",cx);w.input("SystemMonitorX",cx);})?;
                Ok::<(), anyhow::Error>(())
            })?;
            smol::Timer::after(Duration::from_millis(300)).await;
            cx.update(|cx| -> Result<()> {
                ensure!(view.read(cx).search.read(cx).value().contains("SystemMonitorX"),"Ctrl+F/input failed");
                let table=view.read(cx).processes.read(cx).delegate();
                ensure!(!table.rows.is_empty() && (0..table.rows.len()).all(|i|table.row(i).unwrap().name.to_lowercase().contains("systemmonitorx")),"filter failed");
                capture(handle,cx,&directory.join("02-filtered.png"))?;
                handle.update(cx,|_,w,cx|{w.click(("row",0usize),cx);})?;
                Ok::<(), anyhow::Error>(())
            })?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx|->Result<()> {
                ensure!(view.read(cx).processes.read(cx).delegate().selection().is_some(),"process row selection failed");
                handle.update(cx,|_,w,cx|w.click("end",cx))?;
                Ok(())
            })?;
            smol::Timer::after(Duration::from_millis(300)).await;
            cx.update(|cx|->Result<()> {
                capture(handle,cx,&directory.join("02b-end-confirmation.png"))?;
                handle.update(cx,|_,w,cx|{ensure!(w.has_active_dialog(cx),"end task did not request confirmation");w.click("cancel",cx);w.press("ctrl-p",cx);Ok::<(),anyhow::Error>(())})??;
                Ok(())
            })?;
            smol::Timer::after(Duration::from_millis(150)).await;
            cx.update(|cx| -> Result<()> {
                ensure!(view.read(cx).paused,"pause shortcut failed");
                handle.update(cx,|_,w,cx|{w.click("pause",cx);w.click("nav-性能",cx);})?;
                Ok::<(), anyhow::Error>(())
            })?;
            let hidden_delay = cx.update(|cx| view.read(cx).settings.interval_ms.saturating_mul(2).saturating_add(300));
            smol::Timer::after(Duration::from_millis(hidden_delay)).await;
            cx.update(|cx| -> Result<()> {
                ensure!(view.read(cx).page==Page::Performance && !view.read(cx).paused,"nav/resume click failed");
                let app = view.read(cx);
                ensure!(app.process_display_seq.is_some_and(|seq| seq < app.last_seq), "hidden process model was rebuilt while performance page was visible");
                ensure!(app.users_display_seq.is_none() && app.history_display_seq.is_none(), "hidden users/history models were formatted before visiting them");                capture(handle,cx,&directory.join("03-performance.png"))?;
                handle.update(cx,|_,w,cx|{w.click("nav-设备与传感器",cx);})?;
                Ok::<(), anyhow::Error>(())
            })?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                ensure!(view.read(cx).page==Page::Devices,"devices navigation failed");
                ensure!(view.read(cx).hardware.is_some(), "hardware inventory did not arrive");
                ensure!(view.read(cx).directx_inventory.as_ref().is_some_and(|i| !i.adapters.is_empty()), "DirectX inventory did not arrive");
                capture(handle,cx,&directory.join("04-devices.png"))?;
                handle.update(cx,|_,w,cx|{w.click("nav-服务",cx);})?;
                Ok::<(), anyhow::Error>(())
            })?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                ensure!(view.read(cx).page==Page::Services && !view.read(cx).services.read(cx).delegate().rows.is_empty(),"services view empty");
                capture(handle,cx,&directory.join("05-services.png"))?;
                handle.update(cx,|_,w,cx|{w.click("nav-启动应用",cx);})?;
                Ok::<(), anyhow::Error>(())
            })?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                ensure!(view.read(cx).page==Page::Startup,"startup navigation failed");
                capture(handle,cx,&directory.join("06-startup.png"))?;
                handle.update(cx,|_,w,cx|{w.click("nav-设置",cx);})?;
                Ok::<(), anyhow::Error>(())
            })?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                capture(handle,cx,&directory.join("07-settings.png"))?;
                handle.update(cx,|_,w,cx|{w.click("new-task",cx);})?;
                Ok::<(), anyhow::Error>(())
            })?;
            smol::Timer::after(Duration::from_millis(300)).await;
            cx.update(|cx| -> Result<()> {
                capture(handle,cx,&directory.join("08-new-task.png"))?;
                handle.update(cx,|_,w,cx|{w.click("task-run",cx);})?;
                Ok::<(), anyhow::Error>(())
            })?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                ensure!(view.read(cx).message.as_ref().is_some_and(|m|m.contains("启动失败")),"empty task must produce an error");
                handle.update(cx,|_,w,cx|{ensure!(w.has_active_dialog(cx),"failed launch dismissed dialog");w.click("task-cancel",cx);w.resize(size(px(1040.),px(640.)));w.refresh();Ok::<(),anyhow::Error>(())})??;
                Ok::<(), anyhow::Error>(())
            })?;
            smol::Timer::after(Duration::from_millis(300)).await;
            cx.update(|cx| -> Result<()> {capture(handle,cx,&directory.join("09-small-window.png"))?;Ok::<(), anyhow::Error>(())})?;
            for (page, label, filename) in [
                (Page::History, "nav-应用历史", "10-history.png"),
                (Page::Users, "nav-用户", "11-users.png"),
                (Page::Details, "nav-详细信息", "12-details.png"),
            ] {
                cx.update(|cx| handle.update(cx, |_, window, cx| window.click(label, cx)))?;
                smol::Timer::after(Duration::from_millis(300)).await;
                cx.update(|cx| -> Result<()> {
                    ensure!(view.read(cx).page == page, "new navigation failed: {label}");
                    let app = view.read(cx);
                    let displayed = match page { Page::History => app.history_display_seq, Page::Users => app.users_display_seq, Page::Details => app.process_display_seq, _ => None };
                    ensure!(displayed == Some(app.last_seq), "newly visible page did not catch up to the latest live snapshot");                    if page == Page::History {
                        ensure!(!view.read(cx).app_history.read(cx).delegate().rows.is_empty(), "application history empty");
                    }
                    if page == Page::Users {
                        ensure!(!view.read(cx).users.read(cx).delegate().rows.is_empty(), "local logged-in users empty");
                    }
                    if page == Page::Details {
                        ensure!(view.read(cx).processes.read(cx).delegate().detailed, "details columns not enabled");
                    }
                    capture(handle, cx, &directory.join(filename))
                })?;
            }
            cx.update(|cx| handle.update(cx, |_, window, cx| {
                window.press("ctrl-f", cx);
                window.press("ctrl-a",cx);
                window.input(&format!("pid:{}", std::process::id()), cx);
            }))?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                let table = view.read(cx).processes.read(cx).delegate();
                ensure!(table.rows.len() == 1 && table.row(0).is_some_and(|p| p.pid == std::process::id()), "own PID filter did not settle");
                handle.update(cx, |_,window,cx| {
                    window.render_frame(cx);
                    window.click(("row",0usize),cx);
                })?;
                Ok(())
            })?;
            smol::Timer::after(Duration::from_millis(500)).await;
            cx.update(|cx| -> Result<()> {
                let app = view.read(cx);
                ensure!(app.process_details.as_ref().is_some_and(|d| d.pid == std::process::id() && d.command_line.is_some()), "asynchronous own-process details did not arrive: {:?}",app.process_details_error);
                handle.update(cx, |_, window, cx| window.click("toggle-process-details",cx))?;
                capture(handle,cx,&directory.join("13-selected-details.png"))?;
                handle.update(cx, |_, window, cx| window.click("verify-file-signature",cx))?;
                Ok(())
            })?;
            let icon_deadline = std::time::Instant::now() + Duration::from_secs(15);
            loop {
                let icon = cx.update(|cx| view.read(cx).processes.read(cx).delegate().request_process_icon(&std::env::current_exe().unwrap()));
                if let Some(icon) = icon {
                    icon_source = serde_json::json!({"width":icon.width,"height":icon.height,"embedded_resource_fallback":icon.used_embedded_resource});
                    ensure!(icon.width == 32 && icon.height == 32, "native executable icon dimensions differ");
                    ensure!(icon.source_path == std::env::current_exe()?, "executable icon came from a different file");
                    break;
                }
                ensure!(std::time::Instant::now() < icon_deadline, "owned executable icon was not loaded asynchronously");
                smol::Timer::after(Duration::from_millis(100)).await;
            }
            cx.update(|cx| -> Result<()> {
                let stats = view.read(cx).processes.read(cx).delegate().icon_cache_stats().context("icon cache unavailable")?;
                ensure!(stats.ready + stats.failed <= stats.capacity && stats.pending <= stats.queue_capacity + stats.workers && stats.workers == 1, "executable icon cache exceeded its bounds");
                Ok(())
            })?;
            let signature_deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                let done = cx.update(|cx| !view.read(cx).file_signature_loading);
                if done { break; }
                ensure!(std::time::Instant::now() < signature_deadline, "native file signature query did not finish");
                smol::Timer::after(Duration::from_millis(100)).await;
            }
            cx.update(|cx| -> Result<()> {
                let app = view.read(cx);
                let signature = app.file_signature.as_ref().context("native file signature result missing")?;
                ensure!(!signature.revocation_checked, "offline signature result incorrectly claims revocation checked");
                ensure!(signature.file_path == std::env::current_exe()?.canonicalize()?, "signature shown for a different file");
                capture(handle,cx,&directory.join("13b-file-signature.png"))
            })?;            cx.update(|cx| handle.update(cx, |_, window, cx| { window.resize(size(px(1380.),px(860.)));window.click("nav-设置",cx); }))?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                let previous = view.read(cx).settings.always_on_top;
                handle.update(cx, |_, window, cx| {
                    window.click("topmost",cx);
                    window.render_frame(cx);
                })?;
                ensure!(view.read(cx).settings.always_on_top != previous, "topmost did not toggle");
                handle.update(cx, |_, window, cx| window.click("topmost",cx))?;
                ensure!(view.read(cx).settings.always_on_top == previous, "topmost setting did not restore");
                Ok(())
            })?;
            cx.update(|cx| handle.update(cx, |_, window, cx| window.click("nav-进程", cx)))?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                handle.update(cx, |_, window, cx| {
                    window.click("process-columns",cx);
                    window.render_frame(cx);
                    ensure!(window.has_active_dialog(cx), "process column chooser did not open");
                    Ok::<(), anyhow::Error>(())
                })??;
                capture(handle,cx,&directory.join("14-process-columns.png"))?;
                handle.update(cx, |_, window, cx| {
                    window.click("process-columns-close",cx);
                    window.render_frame(cx);
                    ensure!(!window.has_active_dialog(cx), "column chooser did not close");
                    window.click("tree-mode",cx);
                    window.render_frame(cx);
                    Ok::<(), anyhow::Error>(())
                })??;
                ensure!(view.read(cx).processes.read(cx).delegate().tree_mode, "tree mode did not toggle");
                handle.update(cx, |_, window, cx| window.click("tree-mode",cx))?;
                ensure!(!view.read(cx).processes.read(cx).delegate().tree_mode, "tree mode did not restore");
                Ok(())
            })?;
            cx.update(|cx| -> Result<()> {
                let previous = view.read(cx).processes.read(cx).delegate().group_mode;
                handle.update(cx, |_, window, cx| window.click("application-groups",cx))?;
                ensure!(view.read(cx).processes.read(cx).delegate().group_mode != previous, "application grouping control did not toggle");
                handle.update(cx, |_, window, cx| window.click("application-groups",cx))?;
                ensure!(view.read(cx).processes.read(cx).delegate().group_mode == previous, "application grouping control did not restore");
                capture(handle,cx,&directory.join("15-application-groups.png"))
            })?;                        cx.update(|cx| handle.update(cx, |_, window, cx| {
                window.click("nav-端口与连接", cx);
            }))?;
            let port_deadline = std::time::Instant::now() + Duration::from_secs(15);
            loop {
                let ready = cx.update(|cx| view.read(cx).ports_snapshot.is_some() && !view.read(cx).ports_loading);
                if ready { break; }
                ensure!(std::time::Instant::now() < port_deadline, "native port inventory did not arrive");
                smol::Timer::after(Duration::from_millis(100)).await;
            }
            cx.update(|cx| -> Result<()> {
                let app = view.read(cx);
                let ports = app.ports_snapshot.as_ref().context("port snapshot missing")?;
                for (local, protocol) in [(owned_tcp.local_addr()?, system_monitor_x::network_ports::PortProtocol::Tcp),
                    (owned_udp.local_addr()?, system_monitor_x::network_ports::PortProtocol::Udp)] {
                    ensure!(ports.entries.iter().any(|entry| entry.pid == std::process::id()
                        && entry.local == local && entry.protocol == protocol && entry.creation_ticks.is_some()),
                        "owned TCP/UDP port or its exact identity missing from real UI snapshot");
                }
                handle.update(cx, |_, window, cx| {
                    window.press("ctrl-f", cx);
                    window.press("ctrl-a", cx);
                    window.input(&format!("port:{} tcp", owned_tcp.local_addr().unwrap().port()), cx);
                })?;
                Ok(())
            })?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                let table = view.read(cx).ports.read(cx).delegate();
                ensure!(!table.rows.is_empty() && (0..table.rows.len()).all(|row| table.row(row).is_some_and(|entry| entry.local.port() == owned_tcp.local_addr().unwrap().port()
                    && entry.protocol == system_monitor_x::network_ports::PortProtocol::Tcp)), "native typed port query failed");
                capture(handle, cx, &directory.join("16-tcp-port-query.png"))?;
                handle.update(cx, |_, window, cx| {
                    window.press("ctrl-a", cx);
                    window.input(&format!("pid:{} udp", std::process::id()), cx);
                })?;
                Ok(())
            })?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                let table = view.read(cx).ports.read(cx).delegate();
                ensure!(!table.rows.is_empty() && (0..table.rows.len()).all(|row| table.row(row).is_some_and(|entry| entry.pid == std::process::id()
                    && entry.protocol == system_monitor_x::network_ports::PortProtocol::Udp)), "native typed PID/UDP query failed");
                handle.update(cx, |_, window, cx| {
                    window.right_click(("row", 0usize), cx);
                    window.render_frame(cx);
                })?;
                Ok(())
            })?;
            // ContextMenu builds via window.defer after the input update flushes.
            smol::Timer::after(Duration::from_millis(50)).await;
            cx.update(|cx| -> Result<()> {
                handle.update(cx, |_, window, cx| {
                    window.render_frame(cx);
                    ensure!(window.try_find("port-show-process").is_some(), "native port context menu did not open");
                    Ok::<(), anyhow::Error>(())
                })??;
                capture(handle, cx, &directory.join("17-udp-context-menu.png"))?;
                handle.update(cx, |_, window, cx| window.click("port-show-process", cx))?;
                Ok(())
            })?;
            smol::Timer::after(Duration::from_millis(300)).await;
            cx.update(|cx| -> Result<()> {
                let app = view.read(cx);
                ensure!(app.page == Page::Details, "port context action did not open process details");
                let table = app.processes.read(cx).delegate();
                ensure!(table.rows.len() == 1 && table.selection().is_some_and(|process| process.pid == std::process::id() && app.ports_snapshot.as_ref().unwrap().entries.iter().any(|entry| entry.pid == process.pid && entry.creation_ticks == process.start_time_windows_ticks)),
                    "port context action did not select the captured process identity");
                capture(handle, cx, &directory.join("18-details-from-port.png"))?;
                handle.update(cx, |_, window, cx| {
                    window.right_click(("row", 0usize), cx);
                })?;
                Ok(())
            })?;
            smol::Timer::after(Duration::from_millis(50)).await;
            cx.update(|cx| -> Result<()> {
                handle.update(cx, |_, window, cx| {
                    window.render_frame(cx);
                    ensure!(window.try_find("process-query-ports").is_some(), "native process context menu did not open");
                    Ok::<(), anyhow::Error>(())
                })??;
                capture(handle, cx, &directory.join("19-process-context-menu.png"))?;
                handle.update(cx, |_, window, cx| window.click("process-query-ports", cx))?;
                Ok(())
            })?;            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                let app = view.read(cx);
                ensure!(app.page == Page::Ports && app.search.read(cx).value().as_ref() == format!("pid:{}", std::process::id()),
                    "process context action did not query the captured PID");
                let table = app.ports.read(cx).delegate();
                ensure!(!table.rows.is_empty() && (0..table.rows.len()).all(|row| table.row(row).is_some_and(|entry| entry.pid == std::process::id())),
                    "process context PID query contains another owner's endpoint");
                handle.update(cx, |_, window, cx| window.click("nav-进程", cx))?;
                Ok(())
            })?;

            performance = super::qa_performance::measure(handle,view.clone(),cx).await?;
            Ok::<(), anyhow::Error>(())
        }.await.context("native UI smoke");
        let report = match result {
            Ok::<(), anyhow::Error>(()) => serde_json::json!({"passed":true,"renderer":"Windows D3D11","performance":performance,"executable_icon":icon_source,"checks":["live telemetry","process table","Ctrl+F and filter","process selection","end task confirmation and cancel","pause/resume","performance navigation","devices navigation","real hardware and DirectX capabilities","column chooser open/close","process tree toggle and restore","services","startup","settings","new task dialog","empty command error and cancel","minimum window size","history","Windows sessions","extended process columns","asynchronous command line and architecture","topmost toggle and restoration","native responsiveness measurement","hidden page models stay deferred","navigation catches up to live snapshot","on-demand real file signature","application grouping toggle and restore","real owned TCP/UDP ports and identity","native typed port and PID query","native UDP row context menu","port context action selects captured process","process context action queries captured PID","real executable icon from bounded background worker"]}),
            Err(error) => serde_json::json!({"passed":false,"error":format!("{error:#}")}),
        };
        let _ = std::fs::write(directory.join("ui-report.json"),serde_json::to_vec_pretty(&report).unwrap());
        cx.update(|cx|cx.quit());
        if report["passed"] != true { std::process::exit(1); }
    }).detach();
}
