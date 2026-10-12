//! Public documentation pictures: synthetic, anonymous samples rendered by GPUI.
//! These pictures are illustrative UI examples, never native-telemetry evidence.
use super::*;
use anyhow::{Context as _, Result, ensure};
use gpui_kit::test::TestWindowExt;
use system_monitor_x::{
    developer_tools::{SavedSearch, SearchScope},
    gpu::{GpuEngineSample, GpuSample},
    monitor::{DiskSample, ProcessSample},
    network_ports::{IpFamily, PortEntry, PortProtocol, PortSnapshot, TcpState},
    process_diagnostics::{EnumerationStatus, LoadedModule, MemorySummary},
};
use windows::Win32::{
    Foundation::FILETIME,
    System::Threading::{GetCurrentProcess, GetProcessTimes},
};
const DEMO_TICKS: u64 = 133_456_789_123_456_789;
fn sample(pid: u32, name: &str, cpu: f32, memory_mb: u64, gpu: f32) -> ProcessSample {
    ProcessSample {
        pid,
        name: name.into(),
        cpu_usage: cpu,
        memory_bytes: memory_mb * 1024 * 1024,
        working_set_bytes: Some(memory_mb * 1024 * 1024),
        gpu_usage: Some(gpu),
        start_time_windows_ticks: Some(DEMO_TICKS),
        start_time_unix_secs: 1_700_000_000,
        exe: Some(PathBuf::from(format!("C:/Apps/{name}"))),
        status: "运行中".into(),
        ..Default::default()
    }
}
fn demo_snapshot(pid: u32, ticks: u64) -> SystemSnapshot {
    let mut own = sample(pid, "SystemMonitorX.exe", 0.3, 80, 0.0);
    own.start_time_windows_ticks = Some(ticks);
    own.exe = std::env::current_exe().ok();
    SystemSnapshot {
        process_network_available: true,
        cpu_ready: true,
        cpu_usage: 21.5,
        sample_interval_secs: 1.,
        memory_total: 32 * 1024 * 1024 * 1024,
        memory_used: 12 * 1024 * 1024 * 1024,
        memory_available: 20 * 1024 * 1024 * 1024,
        logical_cpus: 16,
        physical_cores: Some(8),
        host_name: "DEMO-PC".into(),
        os_name: "Windows 11".into(),
        architecture: "x64".into(),
        uptime_secs: 10800,
        collector_duration_ms: 4.,
        processes: vec![
            sample(4200, "chrome.exe", 5.2, 780, 12.),
            sample(4400, "Code.exe", 2.1, 420, 0.5),
            sample(4600, "cargo.exe", 12.8, 260, 0.),
            sample(4800, "node.exe", 1.1, 96, 0.),
            own,
        ],
        gpus: vec![GpuSample {
            name: "Example GPU".into(),
            usage_percent: Some(18.),
            engines: vec![GpuEngineSample {
                name: "3D".into(),
                usage_percent: 18.,
            }],
            ..Default::default()
        }],
        disks: vec![DiskSample {
            name: "System".into(),
            id: "demo-volume".into(),
            mount_point: "C:/".into(),
            total_bytes: 512 * 1024 * 1024 * 1024,
            available_bytes: 220 * 1024 * 1024 * 1024,
            active_percent: Some(6.),
            ..Default::default()
        }],
        ..Default::default()
    }
}
fn demo_ports() -> Arc<PortSnapshot> {
    Arc::new(PortSnapshot {
        complete: true,
        collection_duration_ms: 2,
        entries: [
            (4800, "node.exe", 3000, PortProtocol::Tcp),
            (4800, "node.exe", 9229, PortProtocol::Tcp),
            (4400, "Code.exe", 5353, PortProtocol::Udp),
        ]
        .into_iter()
        .map(|(pid, name, port, protocol)| PortEntry {
            pid,
            creation_ticks: Some(DEMO_TICKS),
            process_name: Some(name.into()),
            exe: Some(PathBuf::from(format!("C:/Apps/{name}"))),
            protocol,
            family: IpFamily::V4,
            local: format!("127.0.0.1:{port}").parse().unwrap(),
            remote: None,
            tcp_state: (protocol == PortProtocol::Tcp).then_some(TcpState::Listen),
            identity_note: None,
        })
        .collect(),
        ..Default::default()
    })
}
fn capture(handle: AnyWindowHandle, cx: &mut App, path: &std::path::Path) -> Result<()> {
    handle.update(cx, |_, w, cx| {
        w.render_frame(cx);
        w.render_to_image()?.save(path)?;
        Ok::<(), anyhow::Error>(())
    })?
}
fn update<R>(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut SystemMonitorApp, &mut Window, &mut Context<SystemMonitorApp>) -> R,
) -> Result<R> {
    cx.update(|cx| handle.update(cx, |_, w, cx| view.update(cx, |app, cx| f(app, w, cx))))
}
pub(super) fn start(
    handle: AnyWindowHandle,
    view: Entity<SystemMonitorApp>,
    directory: PathBuf,
    cx: &mut App,
) {
    cx.spawn(async move|cx|{
    let result:Result<()> = async{
        std::fs::create_dir_all(&directory)?;
        // Identity is real only for the sole read-only inspection below. All
        // screenshot-visible process/system/endpoint samples are illustrations.
        let (mut birth,mut exit,mut kernel,mut user)=(FILETIME::default(),FILETIME::default(),FILETIME::default(),FILETIME::default());
        unsafe{GetProcessTimes(GetCurrentProcess(),&mut birth,&mut exit,&mut kernel,&mut user)}?;
        let ticks=(u64::from(birth.dwHighDateTime)<<32)|u64::from(birth.dwLowDateTime);let pid=std::process::id();
        let mut report=smol::unblock(move||system_monitor_x::process_diagnostics::ProcessDiagnostics::read(pid,ticks)).await?;
        report.modules.entries=[("SystemMonitorX.exe","C:/Apps/SystemMonitorX/SystemMonitorX.exe",8*1024*1024),("kernel32.dll","C:/Windows/System32/kernel32.dll",768*1024),("user32.dll","C:/Windows/System32/user32.dll",1024*1024),("vcruntime140.dll","C:/Apps/SystemMonitorX/vcruntime140.dll",120*1024)].into_iter().enumerate().map(|(i,(name,path,size))|LoadedModule{name:name.into(),path:path.into(),path_utf16:path.encode_utf16().collect(),path_encoding_lossy:false,path_may_be_truncated:false,base_address:0x180000000+i as u64*0x01000000,image_size_bytes:size}).collect();
        report.modules.status=EnumerationStatus::Complete;report.modules.issues.clear();report.issues.clear();report.memory.issues.clear();
        report.memory.summary=MemorySummary{committed_bytes:96*1024*1024,reserved_bytes:256*1024*1024,private_committed_bytes:72*1024*1024,executable_committed_bytes:12*1024*1024,..Default::default()};
        report.memory.regions.clear();report.memory.status=EnumerationStatus::Unavailable;report.read_duration_ms=4;
        let demo=Arc::new(demo_snapshot(pid,ticks));let endpoints=demo_ports();
        update(handle,&view,cx,|app,w,cx|{
            app.paused=true;app.stopped.store(true,Ordering::Relaxed);app.snapshot=Some(demo.clone());app.snapshot_sampled_instant=Some(Instant::now());app.last_seq=10_000;app.process_display_seq=None;app.overview_seq=None;
            app.settings.dark=true;Theme::change(ThemeMode::Dark,Some(w),cx);app.settings.group_processes=false;
            app.processes.update(cx,|table,cx|{table.delegate_mut().group_mode=false;table.delegate_mut().tree_mode=false;table.delegate_mut().selected=None;table.clear_selection(cx);});
            app.set_simple_columns(true,cx);w.resize(size(px(1240.),px(820.)));
            app.settings.saved_searches=vec![SavedSearch::new(SearchScope::Ports,"Local development","port:3000").unwrap()];
        })?;
        for (locale,suffix) in [(Language::EnUs,"en"),(Language::ZhCn,"zh")]{
            update(handle,&view,cx,|app,w,cx|{app.set_language(locale,w,cx);app.message=Some(UiMessage::Raw(if locale==Language::ZhCn{"文档示例 · 匿名示例数据，不是本机遥测"}else{"Documentation demo · Illustrative data, not live telemetry"}.into()));app.set_page(Page::Overview,w,cx);})?;
            smol::Timer::after(Duration::from_millis(180)).await;
            cx.update(|cx|capture(handle,cx,&directory.join(format!("overview-{suffix}.png"))))?;
            update(handle,&view,cx,|app,w,cx|app.set_page(Page::Processes,w,cx))?;smol::Timer::after(Duration::from_millis(180)).await;
            cx.update(|cx|capture(handle,cx,&directory.join(format!("processes-{suffix}.png"))))?;
            update(handle,&view,cx,|app,w,cx|app.set_page(Page::Ports,w,cx))?;
            let deadline=Instant::now()+Duration::from_secs(10);while cx.update(|cx|view.read(cx).ports_loading){ensure!(Instant::now()<deadline,"documentation port refresh did not settle");smol::Timer::after(Duration::from_millis(50)).await;}
            update(handle,&view,cx,|app,_,cx|{app.ports_snapshot=Some(endpoints.clone());app.ports.update(cx,|table,cx|{table.delegate_mut().set_snapshot(endpoints.clone());ports::sync_selection(table,cx);cx.notify();});})?;
            smol::Timer::after(Duration::from_millis(180)).await;cx.update(|cx|capture(handle,cx,&directory.join(format!("ports-{suffix}.png"))))?;
            update(handle,&view,cx,|app,w,cx|{app.set_page(Page::Processes,w,cx);app.open_process_tools(pid,ticks,w,cx);})?;
            let deadline=Instant::now()+Duration::from_secs(10);while cx.update(|cx|view.read(cx).inspection_loading){ensure!(Instant::now()<deadline,"own documentation inspection did not settle");smol::Timer::after(Duration::from_millis(50)).await;}
            update(handle,&view,cx,|app,w,cx|{app.inspection=Some(Arc::new(report.clone()));app.inspection_loading=false;app.inspection_error=None;app.inspection_show_memory=false;cx.notify();w.refresh();})?;
            smol::Timer::after(Duration::from_millis(180)).await;
            cx.update(|cx|capture(handle,cx,&directory.join(format!("process-tools-{suffix}.png"))))?;
            update(handle,&view,cx,|_,w,cx|w.close_dialog(cx))?;
        }
        Ok(())
    }.await.context("anonymous native documentation rendering");
    let passed=result.is_ok();let value=serde_json::json!({"passed":passed,"scope":"Synthetic anonymous documentation samples rendered by the native Windows GPUI renderer. Not telemetry acceptance.","screenshot_count":8,"error":result.err().map(|e|format!("{e:#}"))});
    let _=std::fs::write(directory.join("documentation-report.json"),serde_json::to_vec_pretty(&value).unwrap());cx.update(|cx|cx.quit());if !passed{std::process::exit(1);}
 }).detach();
}
