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
        let result: Result<()> = async {
            std::fs::create_dir_all(&directory)?;
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
                ensure!(!table.rows.is_empty() && table.rows.iter().all(|p|p.name.to_lowercase().contains("systemmonitorx")),"filter failed");
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
            smol::Timer::after(Duration::from_millis(300)).await;
            cx.update(|cx| -> Result<()> {
                ensure!(view.read(cx).page==Page::Performance && !view.read(cx).paused,"nav/resume click failed");
                capture(handle,cx,&directory.join("03-performance.png"))?;
                handle.update(cx,|_,w,cx|{w.click("nav-设备与传感器",cx);})?;
                Ok::<(), anyhow::Error>(())
            })?;
            smol::Timer::after(Duration::from_millis(200)).await;
            cx.update(|cx| -> Result<()> {
                ensure!(view.read(cx).page==Page::Devices,"devices navigation failed");
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
            Ok::<(), anyhow::Error>(())
        }.await.context("native UI smoke");
        let report = match result {
            Ok::<(), anyhow::Error>(()) => serde_json::json!({"passed":true,"renderer":"Windows D3D11","checks":["live telemetry","process table","Ctrl+F and filter","pause/resume","performance navigation","devices navigation","services","startup","settings","new task dialog","minimum window size"]}),
            Err(error) => serde_json::json!({"passed":false,"error":format!("{error:#}")}),
        };
        let _ = std::fs::write(directory.join("ui-report.json"),serde_json::to_vec_pretty(&report).unwrap());
        cx.update(|cx|cx.quit());
        if report["passed"] != true { std::process::exit(1); }
    }).detach();
}
