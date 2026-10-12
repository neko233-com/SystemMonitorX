//! Native usability acceptance using the real Windows GPUI window.
//! Only this monitor process is inspected; no terminal, mutation or update is run.
//! Images and failure reports contain local source data and remain private.
use super::{Page, Settings, SystemMonitorApp, localization::UiMessage, ports, tables};
use anyhow::{Context as _, Result, ensure};
use gpui_kit::component::{WindowExt as _, table::TableDelegate};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AsyncApp, Bounds, ClipboardItem, Context, ElementId, Entity, Pixels,
    ScrollDelta, Size, Window, point, px, size,
};
use serde_json::{Value, json};
use std::path::Path;
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use system_monitor_x::process_details::ProcessDetails;
use system_monitor_x::{
    developer_tools::SearchScope,
    i18n::Language,
    insights::{ResourceKind, SnapshotFreshness},
};

const SETTLE: Duration = Duration::from_millis(70);

struct Original {
    settings: Settings,
    page: Page,
    paused: bool,
    message: Option<UiMessage>,
    search: String,
    quick_search: String,
    process_query: String,
    process_selected: Option<(u32, u64)>,
    modes: (bool, bool, bool),
    sort: (usize, bool),
    widths: Vec<Pixels>,
    viewport: Size<Pixels>,
    ports_query: String,
    ports_selected: Option<system_monitor_x::network_ports::PortKey>,
    details: Option<Arc<ProcessDetails>>,
    details_error: Option<UiMessage>,
    details_identity: Option<(u32, u64)>,
    details_expanded: bool,
    inspection: Option<Arc<system_monitor_x::process_diagnostics::ProcessDiagnostics>>,
    inspection_error: Option<UiMessage>,
    inspection_identity: Option<(u32, u64)>,
    inspection_show_memory: bool,
    clipboard: Option<ClipboardItem>,
}
fn update_view<R>(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    cx: &mut AsyncApp,
    update: impl FnOnce(&mut SystemMonitorApp, &mut Window, &mut Context<SystemMonitorApp>) -> Result<R>,
) -> Result<R> {
    cx.update(|cx| handle.update(cx, |_, w, cx| view.update(cx, |app, cx| update(app, w, cx))))?
}
fn contains(parent: Bounds<Pixels>, child: Bounds<Pixels>) -> bool {
    child.left() >= parent.left() - px(1.)
        && child.top() >= parent.top() - px(1.)
        && child.right() <= parent.right() + px(1.)
        && child.bottom() <= parent.bottom() + px(1.)
}
fn has_area(bounds: Bounds<Pixels>) -> bool {
    bounds.size.width > px(0.) && bounds.size.height > px(0.)
}
fn viewport(window: &Window) -> Bounds<Pixels> {
    Bounds::new(point(px(0.), px(0.)), window.viewport_size())
}
fn bounds_json(bounds: Bounds<Pixels>) -> Value {
    json!({"x":bounds.origin.x.as_f32(),"y":bounds.origin.y.as_f32(),"width":bounds.size.width.as_f32(),"height":bounds.size.height.as_f32()})
}
fn press(handle: AnyWindowHandle, key: &str, cx: &mut AsyncApp) -> Result<()> {
    cx.update(|cx| handle.update(cx, |_, w, cx| w.press(key, cx)))?;
    Ok(())
}
fn input_query(handle: AnyWindowHandle, query: &str, cx: &mut AsyncApp) -> Result<()> {
    cx.update(|cx| {
        handle.update(cx, |_, w, cx| {
            w.press("ctrl-f", cx);
            w.press("ctrl-a", cx);
            w.press("backspace", cx);
            w.input(query, cx);
        })
    })?;
    Ok(())
}
/// Every click is gated by observed enabled/full viewport bounds. A scroll
/// parent permits only real bounded wheel input, including reversing overshoot.
async fn click_checked(
    handle: AnyWindowHandle,
    id: impl Into<ElementId>,
    scroller: Option<&'static str>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let id = id.into();
    let mut observation = Value::Null;
    for attempt in 0..=20 {
        let (done, layout) = cx.update(|cx| handle.update(cx, |_, w, cx| -> Result<(bool, Value)> {
            w.render_frame(cx);
            let target = w.try_find(id.clone()).with_context(||format!("usability target missing: {id:?}"))?;
            let bounds = target.bounds(); let visible_viewport = viewport(w);
            let parent = if let Some(parent_id) = scroller {
                let observed = w.try_find(parent_id).with_context(||format!("scroll parent missing: {parent_id}"))?;
                ensure!(observed.visible() && has_area(observed.bounds()), "scroll parent is invisible/empty: {parent_id}");
                ensure!(contains(visible_viewport, observed.bounds()), "scroll parent exceeds viewport: {parent_id}");
                observed.bounds()
            } else { visible_viewport };
            let layout = json!({"id":format!("{id:?}"),"attempt":attempt,"visible":target.visible(),"disabled":target.disabled(),"bounds":bounds_json(bounds),"parent":bounds_json(parent),"viewport":bounds_json(visible_viewport)});
            if target.visible() && has_area(bounds) && contains(parent, bounds) && contains(visible_viewport, bounds) {
                ensure!(target.disabled()!=Some(true), "usability target disabled: {layout}");
                w.click(id.clone(), cx); return Ok((true, layout));
            }
            ensure!(attempt < 20 && scroller.is_some(), "usability target not fully visible: {layout}");
            let delta = if bounds.top() < parent.top() { px(75.) } else { px(-75.) };
            w.scroll(scroller.expect("validated scroll parent"), ScrollDelta::Pixels(point(px(0.), delta)), cx);
            Ok((false, layout))
        }))??;
        observation = layout;
        if done {
            return Ok(observation);
        }
        smol::Timer::after(SETTLE).await;
    }
    anyhow::bail!("usability target remained clipped after bounded wheel input: {observation}")
}
fn capture(handle: AnyWindowHandle, path: &Path, cx: &mut AsyncApp) -> Result<()> {
    cx.update(|cx| {
        handle.update(cx, |_, w, cx| {
            w.render_frame(cx);
            w.render_to_image()?.save(path)?;
            Ok::<(), anyhow::Error>(())
        })
    })??;
    Ok(())
}
fn save_original(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    cx: &mut AsyncApp,
) -> Result<Original> {
    let viewport = cx.update(|cx| {
        handle.update(cx, |_, w, cx| {
            ensure!(
                !w.has_active_dialog(cx),
                "usability QA requires no preceding dialog"
            );
            Ok::<_, anyhow::Error>(w.viewport_size())
        })
    })??;
    cx.update(|cx| -> Result<Original> {
        let app = view.read(cx);
        ensure!(
            !app.operation_busy && !app.inspection_loading,
            "usability QA requires preceding operations to settle"
        );
        let settings = app.settings.clone();
        let table = app.processes.clone();
        let delegate = table.read(cx).delegate();
        let mut original = Original {
            settings: settings.clone(),
            page: app.page,
            paused: app.paused,
            message: app.message.clone(),
            search: app.search.read(cx).value().to_string(),
            quick_search: app.quick_search.read(cx).value().to_string(),
            process_query: delegate.query.clone(),
            process_selected: delegate.selected,
            modes: (delegate.group_mode, delegate.tree_mode, delegate.detailed),
            sort: (delegate.sort_col, delegate.descending),
            widths: Vec::new(),
            viewport,
            ports_query: app.ports.read(cx).delegate().query.clone(),
            ports_selected: app.ports.read(cx).delegate().selected.clone(),
            details: app.process_details.clone(),
            details_error: app.process_details_error.clone(),
            details_identity: app.process_details_identity,
            details_expanded: app.process_details_expanded,
            inspection: app.inspection.clone(),
            inspection_error: app.inspection_error.clone(),
            inspection_identity: app.inspection_identity,
            inspection_show_memory: app.inspection_show_memory,
            clipboard: cx.read_from_clipboard(),
        };
        original.widths = table.update(cx, |table, cx| {
            table.delegate_mut().detailed = true;
            table.delegate_mut().set_hidden_columns(&[]);
            let widths = (0..table.delegate().columns_count(cx))
                .map(|i| table.delegate().column(i, cx).width)
                .collect();
            table.delegate_mut().detailed = original.modes.2;
            table
                .delegate_mut()
                .set_hidden_columns(&settings.hidden_process_columns);
            widths
        });
        Ok(original)
    })
}
fn restore(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    original: &Original,
    cx: &mut AsyncApp,
) -> Result<()> {
    update_view(handle, view, cx, |app, w, cx| {
        if w.has_active_dialog(cx) {
            w.close_dialog(cx);
        }
        app.settings = original.settings.clone();
        app.interval
            .store(original.settings.interval_ms, Ordering::Relaxed);
        app.paused = original.paused;
        app.set_language(original.settings.language, w, cx);
        app.set_page(original.page, w, cx);
        app.search.update(cx, |input, cx| {
            input.set_value(original.search.clone(), w, cx)
        });
        app.quick_search.update(cx, |input, cx| {
            input.set_value(original.quick_search.clone(), w, cx)
        });
        app.filter(cx);
        app.processes.update(cx, |table, cx| {
            let d = table.delegate_mut();
            d.detailed = true;
            d.set_hidden_columns(&[]);
            d.set_column_widths(&original.widths);
            d.group_mode = original.modes.0;
            d.tree_mode = original.modes.1;
            d.detailed = original.modes.2;
            d.set_hidden_columns(&original.settings.hidden_process_columns);
            d.sort_col = original.sort.0;
            d.descending = original.sort.1;
            d.query = original.process_query.clone();
            d.rebuild();
            d.selected = original.process_selected;
            table.refresh(cx);
            tables::sync_selection(table, cx);
            cx.notify();
        });
        app.ports.update(cx, |table, cx| {
            table.delegate_mut().set_query(&original.ports_query);
            table.delegate_mut().selected = original.ports_selected.clone();
            ports::sync_selection(table, cx);
            cx.notify();
        });
        app.process_details_generation = app.process_details_generation.wrapping_add(1);
        app.process_details_loading = false;
        app.process_details = original.details.clone();
        app.process_details_error = original.details_error.clone();
        app.process_details_identity = original.details_identity;
        app.process_details_expanded = original.details_expanded;
        app.inspection_generation = app.inspection_generation.wrapping_add(1);
        app.inspection_loading = false;
        app.inspection = original.inspection.clone();
        app.inspection_error = original.inspection_error.clone();
        app.inspection_identity = original.inspection_identity;
        app.inspection_show_memory = original.inspection_show_memory;
        app.message = original.message.clone();
        app.refresh_overview_age(cx);
        app.settings_writer
            .send(original.settings.clone())
            .map_err(|_| anyhow::anyhow!("settings restoration writer disconnected"))?;
        cx.write_to_clipboard(
            original
                .clipboard
                .clone()
                .unwrap_or_else(|| ClipboardItem::new_string(String::new())),
        );
        w.resize(original.viewport);
        w.refresh();
        cx.notify();
        Ok(())
    })
}
async fn wait_settings(expected: &Settings) -> Result<()> {
    let expected = serde_json::to_value(expected)?;
    let path = Settings::path();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let path = path.clone();
        let saved = smol::unblock(move || {
            std::fs::read(path)
                .ok()
                .and_then(|data| serde_json::from_slice::<Value>(&data).ok())
        })
        .await;
        if saved.as_ref() == Some(&expected) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "usability saved-settings writer did not persist expected preferences"
        );
        smol::Timer::after(SETTLE).await;
    }
}
fn validate_overview(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    cx.update(|cx| -> Result<Value> {
        let app=view.read(cx);ensure!(app.page==Page::Overview,"Overview shortcut did not navigate");
        let overview=app.overview.as_ref().context("real Overview summary missing")?;
        ensure!(overview.observed_age.age_millis.is_some(),"Overview age is unknown despite live sample");
        ensure!(overview.observed_age.paused==app.paused,"Overview pause state differs from display state");
        ensure!(overview.cpu.resource==ResourceKind::Cpu && overview.memory.resource==ResourceKind::Memory && overview.disk.resource==ResourceKind::Disk && overview.gpu.resource==ResourceKind::Gpu,"Overview resources mismatched");
        ensure!(overview.process_count>0,"Overview summary has no real processes");
        ensure!((overview.freshness == SnapshotFreshness::Paused) == app.paused, "Overview freshness did not track native pause state");
        let data=json!({"age_millis":overview.observed_age.age_millis,"paused":app.paused,"freshness":overview.freshness,"process_count":overview.process_count,"resources":[overview.cpu.resource,overview.memory.resource,overview.disk.resource,overview.gpu.resource]});
        let layout=handle.update(cx,|_,w,cx|->Result<Value>{
            w.render_frame(cx);let parent=w.try_find("overview-content").context("native Overview content missing")?.bounds();let v=viewport(w);
            ensure!(contains(v,parent),"Overview scroll viewport clipped");
            let mut metrics=Vec::new();
            for id in ["overview-freshness","overview-age","overview-metric-cpu","overview-metric-memory","overview-metric-disk","overview-metric-gpu"] {
                let e=w.try_find(id).with_context(||format!("Overview observed marker missing: {id}"))?;
                ensure!(e.visible()&&has_area(e.bounds())&&contains(parent,e.bounds())&&contains(v,e.bounds()),"Overview marker clipped: {id} bounds={:?} viewport={v:?}",e.bounds());metrics.push(json!({"id":id,"bounds":bounds_json(e.bounds())}));
            }
            Ok(json!({"logical_viewport":bounds_json(v),"content":bounds_json(parent),"metrics_and_age":metrics}))
        })??;
        Ok(json!({"source":data,"layout":layout}))
    })
}
fn validate_columns(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    common: bool,
    cx: &mut AsyncApp,
) -> Result<Value> {
    cx.update(|cx| -> Result<Value> {
        let app=view.read(cx);let table=app.processes.read(cx);let d=table.delegate();let expected=if common {5}else{12};
        ensure!(app.page==Page::Processes && !d.detailed,"column test left the process page");
        ensure!(d.columns_count(cx)==expected,"column preset shows wrong count");
        ensure!((0..expected).all(|i|d.raw_column(i)==Some(i)),"column preset changed raw column identity");
        let name_width=d.column(0,cx).width.as_f32();let pid_width=d.column(1,cx).width.as_f32();
        let layout=handle.update(cx,|_,w,cx|->Result<Value>{
            w.render_frame(cx);let v=viewport(w);let parent=w.try_find("process-table-region").context("process table region missing")?.bounds();
            let mut headers=Vec::new();for i in 0usize..if common {5}else{2} {
                let e=w.try_find(("col-header",i)).context("native preset header missing")?;
                ensure!(e.visible()&&contains(v,e.bounds())&&contains(parent,e.bounds()),"preset header not inside table/viewport");
                headers.push(json!({"raw_column":i,"bounds":bounds_json(e.bounds())}));
            }
            Ok(json!({"table":bounds_json(parent),"headers":headers}))
        })??;
        Ok(json!({"common":common,"columns":expected,"name_width":name_width,"pid_width":pid_width,"layout":layout}))
    })
}
async fn select_owned(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    query: &str,
    cx: &mut AsyncApp,
) -> Result<(u32, u64)> {
    press(handle, "ctrl-2", cx)?;
    input_query(handle, query, cx)?;
    smol::Timer::after(SETTLE).await;
    let identity = cx.update(|cx| -> Result<_> {
        let d = view.read(cx).processes.read(cx).delegate();
        ensure!(
            d.rows.len() == 1,
            "own PID query did not yield exactly one process"
        );
        let p = d.row(0).context("owned process row absent")?;
        ensure!(p.pid == std::process::id(), "QA selected another process");
        Ok((
            p.pid,
            p.start_time_windows_ticks
                .filter(|ticks| *ticks > 0)
                .context("owned process has no exact creation time")?,
        ))
    })?;
    click_checked(handle, ("row", 0usize), Some("process-table-region"), cx).await?;
    smol::Timer::after(SETTLE).await;
    cx.update(|cx| -> Result<()> {
        ensure!(
            view.read(cx).processes.read(cx).delegate().selected == Some(identity),
            "native own-process row selection lost exact identity"
        );
        Ok(())
    })?;
    Ok(identity)
}
async fn wait_inspection(
    view: &Entity<SystemMonitorApp>,
    identity: (u32, u64),
    cx: &mut AsyncApp,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let report=cx.update(|cx|->Result<Option<Value>>{
            let app=view.read(cx);if app.inspection_loading{return Ok(None);}
            let r=app.inspection.as_ref().with_context(||format!("owned inspection missing; error={:?}",app.inspection_error.as_ref().map(|error|error.render())))?;
            ensure!(app.inspection_identity==Some(identity)&&r.pid==identity.0&&r.creation_ticks==identity.1,"owned inspection changed identity");
            ensure!(!r.modules.entries.is_empty()&&!r.memory.regions.is_empty(),"self inspection did not return actual modules/regions");
            ensure!(r.modules.entries.len()<=4096&&r.memory.regions.len()<=16384,"self inspection exceeds bounded record limits");
            Ok(Some(json!({"module_count":r.modules.entries.len(),"memory_region_count":r.memory.regions.len(),"module_status":r.modules.status,"memory_status":r.memory.status,"read_duration_ms":r.read_duration_ms,"limits":r.limits})))
        })?;
        if let Some(report) = report {
            return Ok(report);
        }
        ensure!(
            Instant::now() < deadline,
            "read-only self inspection did not settle"
        );
        smol::Timer::after(SETTLE).await;
    }
}
async fn open_owned_tools(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    query: &str,
    directory: &Path,
    cx: &mut AsyncApp,
) -> Result<((u32, u64), Value)> {
    let identity = select_owned(handle, view, query, cx).await?;
    super::qa::click_inspector_action(handle, "process-toolkit", directory, cx).await?;
    let report = wait_inspection(view, identity, cx).await?;
    Ok((identity, report))
}
async fn visible_list(
    handle: AnyWindowHandle,
    id: &'static str,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let mut last = Value::Null;
    for attempt in 0..=12 {
        let (visible,layout)=cx.update(|cx|handle.update(cx,|_,w,cx|->Result<(bool,Value)>{
            w.render_frame(cx);let list=w.try_find(id).context("selected inspection tab has no native list")?;
            let parent=w.try_find("process-tools-content").context("inspection scroll parent missing")?.bounds();
            ensure!(contains(viewport(w),parent),"inspection scroll container clipped outside viewport");
            let bounds=list.bounds();let layout=json!({"id":id,"attempt":attempt,"bounds":bounds_json(bounds),"scroll_parent":bounds_json(parent),"scope":"intentional scroll clipping: verify active list intersection, not whole-list containment"});
            if list.visible()&&has_area(bounds)&&bounds.left()<parent.right()&&bounds.right()>parent.left()&&bounds.top()<parent.bottom()-px(8.)&&bounds.bottom()>parent.top()+px(8.) { return Ok((true,layout)); }
            ensure!(attempt<12,"inspection list has no visible intersection: {layout}");
            let delta=if bounds.bottom()<=parent.top()+px(8.){px(75.)}else{px(-75.)};
            w.scroll("process-tools-content",ScrollDelta::Pixels(point(px(0.),delta)),cx);Ok((false,layout))
        }))??;
        last = layout;
        if visible {
            return Ok(last);
        }
        smol::Timer::after(SETTLE).await;
    }
    anyhow::bail!("inspection list remained clipped: {last}")
}
async fn open_owned_context_tools(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    query: &str,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let identity = select_owned(handle, view, query, cx).await?;
    cx.update(|cx| {
        handle.update(cx, |_, w, cx| -> Result<()> {
            w.render_frame(cx);
            let row = w
                .try_find(("row", 0usize))
                .context("owned row missing before context menu")?;
            let parent = w
                .try_find("process-table-region")
                .context("owned table region missing")?
                .bounds();
            ensure!(
                row.visible()
                    && has_area(row.bounds())
                    && contains(parent, row.bounds())
                    && contains(viewport(w), row.bounds()),
                "owned context menu row clipped"
            );
            w.right_click(("row", 0usize), cx);
            Ok(())
        })
    })??;
    smol::Timer::after(SETTLE).await;
    let layout = click_checked(handle, "process-open-tools", None, cx).await?;
    let report = wait_inspection(view, identity, cx).await?;
    Ok(json!({"context_menu":layout,"owned_read_only_inspection":report}))
}

fn clipboard_text(cx: &mut AsyncApp) -> Result<String> {
    cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .context("native clipboard has no copied text")
}

pub(super) async fn measure(
    handle: AnyWindowHandle,
    view: Entity<SystemMonitorApp>,
    directory: &Path,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let output = directory.join("usability-v0.5");
    std::fs::create_dir_all(&output)?;
    let original = save_original(handle, &view, cx)?;
    let began = Instant::now();
    let query = format!("pid:{}", std::process::id());
    let mut checks: Vec<&str> = Vec::new();
    let mut phase = "setup";
    let measured:Result<Value>=async {
        update_view(handle,&view,cx,|app,w,cx|{app.processes.update(cx,|table,cx|{let d=table.delegate_mut();d.group_mode=false;d.tree_mode=false;d.rebuild();table.refresh(cx);tables::sync_selection(table,cx);cx.notify();});w.resize(size(px(1040.),px(640.)));w.refresh();Ok(())})?;
        if cx.update(|cx|view.read(cx).paused){press(handle,"ctrl-p",cx)?;}
        phase="overview";press(handle,"ctrl-1",cx)?;smol::Timer::after(SETTLE).await;
        let overview=validate_overview(handle,&view,cx)?;checks.push("minimum-size Overview shows four real resources and observed age");
        press(handle,"ctrl-p",cx)?;smol::Timer::after(SETTLE).await;
        let before=validate_overview(handle,&view,cx)?;
        ensure!(before["source"]["paused"]==true&&before["source"]["freshness"]=="paused","pause did not update Overview freshness");
        smol::Timer::after(Duration::from_millis(1400)).await;let paused=validate_overview(handle,&view,cx)?;
        ensure!(paused["source"]["age_millis"].as_u64()>before["source"]["age_millis"].as_u64(),"displayed snapshot age did not increase while paused");
        checks.push("native pause marks Overview paused and its original sample age keeps increasing");
        press(handle,"ctrl-p",cx)?;smol::Timer::after(SETTLE).await;
        ensure!(!cx.update(|cx|view.read(cx).paused),"native resume did not resume display");checks.push("native resume restores display");
        phase="quick actions";press(handle,"ctrl-shift-p",cx)?;smol::Timer::after(SETTLE).await;
        cx.update(|cx|handle.update(cx,|_,w,cx|{w.input("Ctrl + 3",cx);}))?;smol::Timer::after(SETTLE).await;
        ensure!(cx.update(|cx|view.read(cx).quick_search.read(cx).value().as_ref()=="Ctrl + 3"),"quick actions input did not receive shortcut text");
        let quick=click_checked(handle,"quick-action-0",Some("quick-action-results"),cx).await?;smol::Timer::after(SETTLE).await;
        ensure!(cx.update(|cx|view.read(cx).page==Page::Ports),"shortcut search did not route to Ports");checks.push("Ctrl+Shift+P searches Ctrl + 3 and native action opens Ports");
        let before_refresh=cx.update(|cx|view.read(cx).ports_last_refresh);press(handle,"f5",cx)?;smol::Timer::after(SETTLE).await;
        ensure!(cx.update(|cx|view.read(cx).ports_last_refresh.is_some_and(|instant|before_refresh.is_none_or(|previous|instant>previous))),"F5 did not request a new port refresh");checks.push("F5 requests current page port refresh");
        phase="help";press(handle,"f1",cx)?;smol::Timer::after(SETTLE).await;
        ensure!(cx.update(|cx|handle.update(cx,|_,w,cx|w.has_active_dialog(cx)))?,"F1 did not open help");
        let help=click_checked(handle,"help-close",None,cx).await?;checks.push("F1 opens native help and visible Close dismisses it");
        phase="saved queries";let identity=select_owned(handle,&view,&query,cx).await?;
        click_checked(handle,"save-search",Some("saved-searches"),cx).await?;smol::Timer::after(SETTLE).await;
        let saved=cx.update(|cx|->Result<Settings>{let s=&view.read(cx).settings;ensure!(s.saved_searches.len()<=8&&s.saved_searches.iter().any(|v|v.scope==SearchScope::Processes&&v.query==query),"own saved query missing or cap exceeded");Ok(s.clone())})?;
        wait_settings(&saved).await?;checks.push("native Save query persists own PID filter within global cap eight");
        input_query(handle,"",cx)?;smol::Timer::after(SETTLE).await;
        let saved_index=cx.update(|cx|view.read(cx).settings.saved_searches.iter().filter(|v|v.scope==SearchScope::Processes).position(|v|v.query==query)).context("owned saved query chip missing")?;
        click_checked(handle,format!("saved-query-{saved_index}"),Some("saved-searches"),cx).await?;smol::Timer::after(SETTLE).await;
        ensure!(cx.update(|cx|view.read(cx).search.read(cx).value().as_ref()==query&&view.read(cx).processes.read(cx).delegate().rows.len()==1),"saved chip did not restore exact query");checks.push("native saved-query chip reuses own PID search");
        click_checked(handle,format!("delete-query-{saved_index}"),Some("saved-searches"),cx).await?;smol::Timer::after(SETTLE).await;
        ensure!(cx.update(|cx|!view.read(cx).settings.saved_searches.iter().any(|v|v.scope==SearchScope::Processes&&v.query==query)),"delete saved query retained fixture");checks.push("native delete removes only the saved query fixture");
        phase="column presets";click_checked(handle,"process-columns",None,cx).await?;smol::Timer::after(SETTLE).await;
        click_checked(handle,"columns-simple",None,cx).await?;click_checked(handle,"process-columns-close",None,cx).await?;
        let common=validate_columns(handle,&view,true,cx)?;checks.push("common-column preset exposes five exact raw columns and contained native headers");
        click_checked(handle,"process-columns",None,cx).await?;smol::Timer::after(SETTLE).await;
        click_checked(handle,"columns-all",None,cx).await?;click_checked(handle,"process-columns-close",None,cx).await?;
        let all=validate_columns(handle,&view,false,cx)?;
        ensure!(all["name_width"]==common["name_width"]&&all["pid_width"]==common["pid_width"],"column preset reset raw name/PID widths");checks.push("all-column preset restores process-page columns and retains raw widths");
        phase="process tools and clipboard";let (tools_identity,inspection)=open_owned_tools(handle,&view,&query,&output,cx).await?;
        ensure!(tools_identity==identity,"own process identity changed during QA");checks.push("native process toolkit performs bounded on-demand modules and region reads for own identity");
        click_checked(handle,"copy-process-query",Some("process-tools-content"),cx).await?;
        let process_command=clipboard_text(cx)?;ensure!(process_command.contains(&format!("Get-Process -Id {}",identity.0))&&process_command.contains(&format!("[long]{}",identity.1))&&!process_command.contains("Stop-Process"),"copied query has wrong identity or a mutation");checks.push("native copy query uses own PID and exact creation time without executing it");
        click_checked(handle,"copy-port-query",Some("process-tools-content"),cx).await?;
        let ports_command=clipboard_text(cx)?;ensure!(ports_command.contains("Get-NetTCPConnection")&&ports_command.contains("Get-NetUDPEndpoint")&&ports_command.contains(&format!("[long]{}",identity.1)),"copied port query did not retain exact owner");checks.push("native copied TCP/UDP queries retain exact owner identity");
        cx.update(|cx|cx.write_to_clipboard(ClipboardItem::new_string("SystemMonitorX owned JSON QA sentinel".into())));
        click_checked(handle,"copy-diagnostic-json",None,cx).await?;let deadline=Instant::now()+Duration::from_secs(5);
        let diagnostic=loop {
            let text=clipboard_text(cx)?;if let Ok(value)=serde_json::from_str::<Value>(&text){break value;}
            ensure!(Instant::now()<deadline,"native diagnostic JSON copy did not settle");smol::Timer::after(SETTLE).await;
        };
        let expected_ticks = identity.1.to_string();
        ensure!(diagnostic["schema"]=="systemmonitorx.process_diagnostic"&&diagnostic["schema_version"]==1,"copied report schema incorrect");
        ensure!(diagnostic["identity"]["pid"]==identity.0&&diagnostic["identity"]["creation_time_windows_ticks"].as_str()==Some(expected_ticks.as_str()),"copied report lost exact PID/FILETIME identity");
        ensure!(diagnostic["privacy"]["command_line_included"]==false&&diagnostic["privacy"]["environment_included"]==false&&diagnostic["details"]["command_line"].is_null(),"default report exposed command line or environment");
        ensure!(diagnostic["privacy"]["unrelated_processes_included"]==false&&diagnostic["endpoints"]["traffic_measurement"]==false,"diagnostic export misstates scope/endpoint semantics");
        ensure!(diagnostic["inspection"]["data"]["creation_ticks"].as_str() == Some(expected_ticks.as_str()), "attached inspection lost exact decimal creation ticks");
        ensure!(diagnostic["inspection"]["data"]["pid"]==identity.0,"JSON inspection owner mismatch");
        if let Some(entries)=diagnostic["endpoints"]["entries"].as_array(){ensure!(entries.iter().all(|e|e["identity"]["pid"]==identity.0&&e["identity"]["creation_time_windows_ticks"].as_str()==Some(expected_ticks.as_str())),"JSON joined wrong/reused endpoint owner");}
        checks.push("native copied diagnostic JSON uses fixed schema and exact identity");checks.push("native JSON excludes command lines/environment/unrelated processes and keeps endpoint-not-traffic semantics");
        click_checked(handle,"process-tools-close",None,cx).await?;
        let owned_context_tools = open_owned_context_tools(handle, &view, &query, cx).await?;
        checks.push("native owned-row context menu opens exact-identity read-only process tools");
        click_checked(handle, "process-tools-close", None, cx).await?;
        phase="eight-locale new views";let mut languages=Vec::new();
        for language in Language::SUPPORTED {
            update_view(handle,&view,cx,|app,w,cx|{app.set_language(language,w,cx);Ok(())})?;
            press(handle,"ctrl-1",cx)?;smol::Timer::after(SETTLE).await;let overview_layout=validate_overview(handle,&view,cx)?;
            capture(handle,&output.join(format!("{}-overview.png",language.tag())),cx)?;
            let (current,report)=open_owned_tools(handle,&view,&query,&output,cx).await?;ensure!(current==identity,"locale toolkit substituted owner identity");
            let modules_tab=click_checked(handle,"inspection-modules-tab",Some("process-tools-content"),cx).await?;smol::Timer::after(SETTLE).await;
            let modules=visible_list(handle,"module-list",cx).await?;
            ensure!(cx.update(|cx|!view.read(cx).inspection_show_memory),"native modules tab did not select modules");
            capture(handle,&output.join(format!("{}-modules.png",language.tag())),cx)?;
            let memory_tab=click_checked(handle,"inspection-memory-tab",Some("process-tools-content"),cx).await?;smol::Timer::after(SETTLE).await;
            let memory=visible_list(handle,"memory-region-list",cx).await?;
            ensure!(cx.update(|cx|view.read(cx).inspection_show_memory),"native memory tab did not select regions");
            capture(handle,&output.join(format!("{}-memory-regions.png",language.tag())),cx)?;
            let active_locale=cx.update(|cx|view.read(cx).resolved_language);ensure!(active_locale==language,"new-view screenshot locale mismatched");
            languages.push(json!({"locale":language.tag(),"locale_switch":"programmatic; native Settings button switching is tested separately","overview":overview_layout,"inspection":report,"modules_tab":modules_tab,"modules_list":modules,"memory_tab":memory_tab,"memory_list":memory,"screenshots":[format!("{}-overview.png",language.tag()),format!("{}-modules.png",language.tag()),format!("{}-memory-regions.png",language.tag())]}));
            click_checked(handle,"process-tools-close",None,cx).await?;
        }
        checks.push("eight locales show native Overview four-resource/age layouts at minimum size");checks.push("eight locales select actual modules and memory-region tabs through visible bounded wheel targets");checks.push("eight locales retain exact own process identity across new-view screenshots");
        Ok(json!({"passed":true,"checks":checks,"measurement":{"duration_ms":began.elapsed().as_millis(),"renderer":"native Windows GPUI/D3D11","logical_viewport":{"width":1040,"height":640},"original_overview":overview,"paused_age":paused,"quick_action":quick,"help_close":help,"common_columns":common,"all_columns":all,"own_inspection":inspection,"owned_context_tools":owned_context_tools,"clipboard_queries_executed":false,"terminal_launched":false,"processes_terminated":false,"updates_installed":false,"json_schema":diagnostic["schema"],"json_schema_version":diagnostic["schema_version"],"default_command_line_excluded":true,"environment_excluded":true,"screenshot_scope":"layout and native action assertions; pixel/glyph review remains separate"},"languages":languages,"screenshot_count":24}))
    }.await;
    if let Err(error) = &measured {
        let _ = capture(handle, &output.join("failure.png"), cx);
        let _ = std::fs::write(
            output.join("failure.json"),
            serde_json::to_vec_pretty(
                &json!({"phase":phase,"error":format!("{error:#}"),"completed_checks":checks,"native_viewport":"1040x640; screenshot captured before restoration"}),
            )?,
        );
    }
    let cleanup = restore(handle, &view, &original, cx);
    let persisted = if cleanup.is_ok() {
        wait_settings(&original.settings).await
    } else {
        Ok(())
    };
    let mut result = measured?;
    cleanup?;
    persisted?;
    let state_restored = cx.update(|cx| -> Result<bool> {
        let app = view.read(cx);
        Ok(
            serde_json::to_value(&app.settings)? == serde_json::to_value(&original.settings)?
                && app.page == original.page
                && app.paused == original.paused
                && app.search.read(cx).value().as_ref() == original.search,
        )
    })?;
    ensure!(
        state_restored,
        "usability QA failed to restore original preferences/page/pause/query"
    );
    result["restoration"] = json!({"settings_and_saved_queries":true,"persisted_settings":true,"page_pause_search":true,"language_and_raw_columns":true,"clipboard_supported_payload_restored":original.clipboard.is_some(),"clipboard_scope":"GPUI-supported clipboard payload only; arbitrary native-only formats are not claimed","failure_cleanup":"attempted before propagating any measurement error"});
    Ok(result)
}
