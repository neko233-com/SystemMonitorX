//! Eight-locale acceptance against the existing native Windows window.
//! Fixtures use this process and local sockets only; no process control or UAC.
//! Screenshots are private QA artifacts, not public release documentation.
use super::{
    Page, Settings, SystemMonitorApp,
    localization::{UiMessage, mf},
    tables,
};
use anyhow::{Context as _, Result, ensure};
use gpui_kit::component::{WindowExt as _, table::TableDelegate};
use gpui_kit::test::{ElementSnapshot, TestWindowExt};
use gpui_kit::{
    AnyWindowHandle, AsyncApp, Bounds, Context, ElementId, Entity, Pixels, Size, Window, px, size,
};
use serde_json::{Value, json};
use std::{
    net::{TcpListener, UdpSocket},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use system_monitor_x::{
    hardware::HardwareInventory,
    i18n::{self, Language},
    network_ports::PortProtocol,
};

const RAW_DIAGNOSTIC: &str = r"QA diagnostic · C:\国际-é-日本\App.exe · E=5 · %{pid} · {error:#}";
const SETTLE: Duration = Duration::from_millis(50);

struct Original {
    settings: Settings,
    page: Page,
    paused: bool,
    message: Option<UiMessage>,
    search: String,
    process_query: String,
    ports_query: String,
    ports_selected: Option<system_monitor_x::network_ports::PortKey>,
    services_query: String,
    services_selected: Option<Vec<String>>,
    process_selected: Option<(u32, u64)>,
    process_modes: (bool, bool, bool),
    process_sort: (usize, bool),
    raw_widths: Vec<Pixels>,
    viewport: Size<Pixels>,
    details: Option<Arc<system_monitor_x::process_details::ProcessDetails>>,
    details_error: Option<UiMessage>,
    details_identity: Option<(u32, u64)>,
    details_expanded: bool,
    signature: Option<Arc<system_monitor_x::file_signature::FileSignature>>,
    signature_error: Option<UiMessage>,
    hardware: Arc<HardwareInventory>,
}

fn update_view<R>(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    cx: &mut AsyncApp,
    update: impl FnOnce(&mut SystemMonitorApp, &mut Window, &mut Context<SystemMonitorApp>) -> Result<R>,
) -> Result<R> {
    cx.update(|cx| {
        handle.update(cx, |_, window, cx| {
            view.update(cx, |app, cx| update(app, window, cx))
        })
    })?
}

fn click(handle: AnyWindowHandle, id: impl Into<ElementId>, cx: &mut AsyncApp) -> Result<()> {
    let id = id.into();
    let scrolling_navigation = matches!(&id,ElementId::Name(name) if name.starts_with("nav-") && name.as_ref() != "nav-settings");
    cx.update(|cx| {
        handle.update(cx, |_, window, cx| {
            if scrolling_navigation {
                const MAX_WHEEL_STEPS: usize = 12;
                for attempt in 0..=MAX_WHEEL_STEPS {
                    window.render_frame(cx);
                    let parent = observed_bounds(window,"sidebar-nav")?;
                    let observed = window.try_find(id.clone())
                        .with_context(||format!("native navigation target {id:?} missing at wheel attempt {attempt}; sidebar={parent:?}"))?;
                    let target = observed.bounds();
                    let viewport = Bounds::new(gpui_kit::point(px(0.),px(0.)),window.viewport_size());
                    ensure!(contains_bounds(viewport,parent),"native sidebar scroll region extends outside viewport: parent={parent:?}, viewport={viewport:?}");
                    if observed.visible() && contains_bounds(parent,target) && contains_bounds(viewport,target) {
                        ensure!(observed.disabled()!=Some(true),"native navigation target is disabled: {id:?}");
                        window.click(id.clone(),cx);
                        return Ok(());
                    }
                    ensure!(attempt<MAX_WHEEL_STEPS,
                        "native navigation target {id:?} not fully inside sidebar after {MAX_WHEEL_STEPS} real wheel steps: target={target:?}, sidebar={parent:?}, viewport={viewport:?}, visible={}",observed.visible());
                    // Follow the observed target position. The fixed Settings
                    // button never participates in this sidebar scroll region.
                    let delta_y = if target.top()<parent.top() {
                        px(80.)
                    } else if target.bottom()>parent.bottom() {
                        px(-80.)
                    } else {
                        anyhow::bail!("native navigation target {id:?} has invisible/clipped bounds despite being inside its sidebar: target={target:?}, sidebar={parent:?}");
                    };
                    window.scroll("sidebar-nav",gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.),delta_y)),cx);
                }
            }
            window.render_frame(cx);
            let observed = window
                .try_find(id.clone())
                .with_context(|| format!("native locale QA target missing: {id:?}"))?;
            ensure!(
                observed.visible(),
                "native locale QA target is not visible: {id:?}"
            );
            ensure!(
                observed.disabled() != Some(true),
                "native locale QA target is disabled: {id:?}"
            );
            window.click(id, cx);
            Ok(())
        })
    })?
}
fn element_layout(observed: Option<ElementSnapshot>, viewport: Size<Pixels>) -> Value {
    let Some(element) = observed else {
        return json!({"observed":false});
    };
    let bounds = element.bounds();
    let inside = bounds.left() >= px(-1.)
        && bounds.top() >= px(-1.)
        && bounds.right() <= viewport.width + px(1.)
        && bounds.bottom() <= viewport.height + px(1.);
    json!({"observed":true,"visible":element.visible(),"fully_inside_logical_viewport":inside,
        "bounds":{"x":bounds.origin.x.as_f32(),"y":bounds.origin.y.as_f32(),"width":bounds.size.width.as_f32(),"height":bounds.size.height.as_f32()},
        "disabled":element.disabled(),"path":format!("{:?}",element.path())})
}

fn observed_bounds(window: &Window, id: &'static str) -> Result<Bounds<Pixels>> {
    let element = window
        .try_find(id)
        .with_context(|| format!("layout container/label {id} was not observed"))?;
    let bounds = element.bounds();
    ensure!(
        element.visible() && bounds.size.width > px(0.) && bounds.size.height > px(0.),
        "layout container/label {id} is not visible or has empty bounds"
    );
    Ok(bounds)
}
fn contains_bounds(parent: Bounds<Pixels>, child: Bounds<Pixels>) -> bool {
    child.left() >= parent.left() - px(1.)
        && child.top() >= parent.top() - px(1.)
        && child.right() <= parent.right() + px(1.)
        && child.bottom() <= parent.bottom() + px(1.)
}
/// Viewport visibility alone does not detect a wrapped button extending past
/// a flex container and being covered by the next sibling's diagnostics.
fn validate_process_regions(window: &Window) -> Result<Value> {
    let actions = observed_bounds(window, "process-actions")?;
    for id in [
        "process-columns",
        "application-groups",
        "tree-mode",
        "export",
        "end",
    ] {
        let button = observed_bounds(window, id)?;
        ensure!(
            contains_bounds(actions, button),
            "process action {id} extends beyond process-actions container: child={button:?}, parent={actions:?}"
        );
    }
    let mut previous_name = "process-actions";
    let mut previous = actions;
    let mut order = Vec::new();
    if let Some(diagnostics) = window.try_find("process-diagnostics-scroll") {
        let bounds = diagnostics.bounds();
        ensure!(
            diagnostics.visible() && bounds.top() >= previous.bottom() - px(1.),
            "process diagnostics overlap or hide process-actions: actions={previous:?}, diagnostics={bounds:?}"
        );
        order.push(json!({"before":previous_name,"after":"process-diagnostics-scroll","gap_px":(bounds.top()-previous.bottom()).as_f32()}));
        previous_name = "process-diagnostics-scroll";
        previous = bounds;
    }
    let table = observed_bounds(window, "process-table-region")?;
    ensure!(
        table.top() >= previous.bottom() - px(1.),
        "process table overlaps {previous_name}: previous={previous:?}, table={table:?}"
    );
    order.push(json!({"before":previous_name,"after":"process-table-region","gap_px":(table.top()-previous.bottom()).as_f32()}));
    if let Some(inspector) = window.try_find("process-inspector-scroll") {
        let bounds = inspector.bounds();
        ensure!(
            inspector.visible() && bounds.top() >= table.bottom() - px(1.),
            "process inspector overlaps the process table: table={table:?}, inspector={bounds:?}"
        );
        order.push(json!({"before":"process-table-region","after":"process-inspector-scroll","gap_px":(bounds.top()-table.bottom()).as_f32()}));
    }
    let footer = observed_bounds(window, "sidebar-footer")?;
    let permission = observed_bounds(window, "sidebar-permission")?;
    ensure!(
        contains_bounds(footer, permission),
        "sidebar permission label exceeds its footer: label={permission:?}, footer={footer:?}"
    );
    let viewport = Bounds::new(gpui_kit::point(px(0.), px(0.)), window.viewport_size());
    ensure!(
        contains_bounds(viewport, footer) && contains_bounds(viewport, permission),
        "sidebar footer/permission label exceeds the minimum-size viewport"
    );
    Ok(
        json!({"process_action_buttons_contained":true,"vertical_sibling_order":order,
        "sidebar_permission_contained_in_footer":true,"sidebar_footer_inside_viewport":true,
        "tolerance_logical_px":1.0,"scope":"Observed layout boxes and sibling ordering; visible glyph overflow/occlusion still needs screenshot review"}),
    )
}
/// Check actual observed layout before touching the row. Failure artifacts are
/// captured at the failing minimum-size frame, before any restoration redraw.
fn click_process_checked(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    language: Language,
    output: &Path,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let model = cx.update(|cx| {
        let app = view.read(cx);
        let table = app.processes.read(cx);
        let delegate = table.delegate();
        let visible = table.visible_range();
        json!({"page":app.page.id(),"locale":app.resolved_language.tag(),"paused":app.paused,
            "search_input":app.search.read(cx).value().as_ref(),"model_query":delegate.query,
            "model_rows":delegate.rows.len(),"native_selected_row":table.selected_row(),"model_selected_identity":delegate.selected,
            "virtualized_rows":{"start":visible.rows().start,"end":visible.rows().end},
            "virtualized_columns":{"start":visible.cols().start,"end":visible.cols().end},
            "model_first_row":delegate.row(0).map(|process|json!({"pid":process.pid,"is_owned_pid":process.pid==std::process::id(),"exact_creation_available":process.start_time_windows_ticks.is_some()})),
            "delegate_headers":table.headers(cx)})
    });
    let layout = cx.update(|cx| handle.update(cx, |_, window, cx| {
        window.render_frame(cx);
        let viewport = window.viewport_size();
        let mut elements = serde_json::Map::new();
        for id in ["pause","new-task","process-columns","application-groups","tree-mode","export","end",
            "process-table-region","process-inspector-scroll","process-diagnostics-scroll","process-actions","sidebar-footer","sidebar-permission"] {
            elements.insert(id.into(),element_layout(window.try_find(id),viewport));
        }
        for (name,id) in [("first_row",("row",0usize)),("name_header",("col-header",0usize)),("pid_header",("col-header",1usize))] {
            elements.insert(name.into(),element_layout(window.try_find(id),viewport));
        }
        json!({"viewport":{"logical_width":viewport.width.as_f32(),"logical_height":viewport.height.as_f32(),"scale_factor":window.scale_factor()},"elements":elements})
    }))?;
    let checked = cx.update(|cx| handle.update(cx, |_, window, cx| -> Result<Value> {
        // Only top-level controls are required to stay in the viewport. The
        // process inspector deliberately scrolls; no bottom action inside it
        // is required to be visible until its own scroller is moved.
        for name in ["pause","new-task","process-columns","application-groups","tree-mode","export","end","name_header","pid_header"] {
            let element=&layout["elements"][name];
            ensure!(element["observed"]==true && element["visible"]==true && element["fully_inside_logical_viewport"]==true,
                "top-level process control/header {name} is missing, clipped or outside the minimum-size viewport");
        }
        let table=&layout["elements"]["process-table-region"];
        ensure!(table["observed"]==true && table["visible"]==true,
            "process table region is missing or not visible in the minimum-size window");
        let row=window.try_find(("row",0usize)).context("owned process row 0 has no native observed element")?;
        let bounds=row.bounds();let viewport=window.viewport_size();
        ensure!(row.visible() && bounds.size.height>px(0.) && bounds.top()>=px(-1.) && bounds.bottom()<=viewport.height+px(1.)
            && bounds.right()>px(0.) && bounds.left()<viewport.width,
            "owned process row 0 is clipped vertically or outside the minimum-size viewport");
        ensure!(model["model_rows"]==1 && model["model_first_row"]["is_owned_pid"]==true,
            "process click target does not represent exactly this process");
        let relationships = validate_process_regions(window)?;
        window.click(("row",0usize),cx);Ok(relationships)
    }))?;
    let relationships = checked.as_ref().ok().cloned();
    let diagnostic = json!({"locale":language.tag(),"stage":"before_owned_process_native_click","model":model,"layout":layout,"region_relationships":relationships});
    if let Err(error) = checked {
        let png = output.join(format!("{}-process-click-failure.png", language.tag()));
        let json_path = output.join(format!("{}-process-click-failure.json", language.tag()));
        let screenshot_error = capture(handle, &png, cx)
            .err()
            .map(|error| format!("{error:#}"));
        let mut failure = diagnostic.clone();
        failure["failure"] = json!(format!("{error:#}"));
        failure["screenshot_error"] = json!(screenshot_error);
        let write_error = serde_json::to_vec_pretty(&failure)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| std::fs::write(&json_path, bytes).map_err(anyhow::Error::from))
            .err()
            .map(|error| format!("{error:#}"));
        return Err(error).context(format!("locale {} process click failed; model/layout={diagnostic}; failure PNG={} (capture error={screenshot_error:?}); JSON={} (write error={write_error:?})",
            language.tag(),png.display(),json_path.display()));
    }
    Ok(diagnostic)
}
fn replace_search(handle: AnyWindowHandle, query: &str, cx: &mut AsyncApp) -> Result<()> {
    cx.update(|cx| {
        handle.update(cx, |_, window, cx| {
            window.press("ctrl-f", cx);
            window.press("ctrl-a", cx);
            window.press("backspace", cx);
            window.input(query, cx);
        })
    })
}

fn capture(handle: AnyWindowHandle, path: &Path, cx: &mut AsyncApp) -> Result<()> {
    cx.update(|cx| {
        handle.update(cx, |_, window, cx| {
            window.render_frame(cx);
            window.render_to_image()?.save(path)?;
            Ok(())
        })
    })?
}

fn save_original(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    cx: &mut AsyncApp,
) -> Result<Original> {
    let viewport = cx.update(|cx| {
        handle.update(cx, |_, window, cx| {
            ensure!(
                !window.has_active_dialog(cx),
                "locale QA requires the preceding dialog to be closed"
            );
            Ok(window.viewport_size())
        })
    })??;
    cx.update(|cx| -> Result<Original> {
        let app = view.read(cx);
        ensure!(
            !app.operation_busy && !app.process_details_loading && !app.file_signature_loading,
            "locale QA requires preceding asynchronous operations to settle"
        );
        let hardware = app
            .hardware
            .clone()
            .context("real cached hardware inventory unavailable")?;
        let table = app.processes.clone();
        let delegate = table.read(cx).delegate();
        let settings = app.settings.clone();
        let mut original = Original {
            settings: settings.clone(),
            page: app.page,
            paused: app.paused,
            message: app.message.clone(),
            search: app.search.read(cx).value().to_string(),
            process_query: delegate.query.clone(),
            ports_query: app.ports.read(cx).delegate().query.clone(),
            ports_selected: app.ports.read(cx).delegate().selected.clone(),
            services_query: app.services.read(cx).delegate().query.clone(),
            services_selected: app
                .services
                .read(cx)
                .selected_row()
                .and_then(|index| app.services.read(cx).delegate().rows.get(index))
                .cloned(),
            process_selected: delegate.selected,
            process_modes: (delegate.group_mode, delegate.tree_mode, delegate.detailed),
            process_sort: (delegate.sort_col, delegate.descending),
            raw_widths: Vec::new(),
            viewport,
            details: app.process_details.clone(),
            details_error: app.process_details_error.clone(),
            details_identity: app.process_details_identity,
            details_expanded: app.process_details_expanded,
            signature: app.file_signature.clone(),
            signature_error: app.file_signature_error.clone(),
            hardware,
        };
        // Read hidden raw-column widths through the public delegate API. No
        // frame is drawn in this transient all-column geometry.
        original.raw_widths = table.update(cx, |table, cx| {
            table.delegate_mut().detailed = true;
            table.delegate_mut().set_hidden_columns(&[]);
            let widths = (0..table.delegate().columns_count(cx))
                .map(|index| table.delegate().column(index, cx).width)
                .collect();
            table.delegate_mut().detailed = original.process_modes.2;
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
    update_view(handle, view, cx, |app, window, cx| {
        // Only QA's harmless default dialog can remain open after an error.
        if window.has_active_dialog(cx) {
            window.close_dialog(cx);
        }
        app.settings = original.settings.clone();
        app.set_language(original.settings.language, window, cx);
        app.set_page(original.page, window, cx);
        app.search.update(cx, |input, cx| {
            input.set_value(original.search.clone(), window, cx)
        });
        app.filter(cx);
        app.processes.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.detailed = true;
            delegate.set_hidden_columns(&[]);
            delegate.set_column_widths(&original.raw_widths);
            delegate.group_mode = original.process_modes.0;
            delegate.tree_mode = original.process_modes.1;
            delegate.detailed = original.process_modes.2;
            delegate.set_hidden_columns(&original.settings.hidden_process_columns);
            delegate.sort_col = original.process_sort.0;
            delegate.descending = original.process_sort.1;
            delegate.query = original.process_query.clone();
            delegate.rebuild();
            delegate.selected = original.process_selected;
            table.refresh(cx);
            tables::sync_selection(table, cx);
            cx.notify();
        });
        app.ports.update(cx, |table, cx| {
            table.delegate_mut().set_query(&original.ports_query);
            table.delegate_mut().selected = original.ports_selected.clone();
            super::ports::sync_selection(table, cx);
            cx.notify();
        });
        app.services.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.query = original.services_query.clone();
            delegate.rebuild();
            let row = original
                .services_selected
                .as_ref()
                .and_then(|previous| delegate.rows.iter().position(|row| row == previous));
            delegate.select_row(row);
            tables::sync_text_selection(table, cx);
            cx.notify();
        });
        app.process_details_generation = app.process_details_generation.wrapping_add(1);
        app.process_details = original.details.clone();
        app.process_details_error = original.details_error.clone();
        app.process_details_identity = original.details_identity;
        app.process_details_expanded = original.details_expanded;
        app.process_details_loading = false;
        app.file_signature_generation = app.file_signature_generation.wrapping_add(1);
        app.file_signature = original.signature.clone();
        app.file_signature_error = original.signature_error.clone();
        app.file_signature_loading = false;
        app.message = original.message.clone();
        app.paused = original.paused;
        window.resize(original.viewport);
        window.refresh();
        cx.notify();
        Ok(())
    })
}

fn check_feedback(
    app: &SystemMonitorApp,
    language: Language,
    diagnostic: &UiMessage,
) -> Result<String> {
    ensure!(
        app.settings.language == language && app.resolved_language == language,
        "language preference and resolved UI locale differ from {}",
        language.tag()
    );
    ensure!(
        i18n::current_language() == language,
        "runtime locale differs from native button preference"
    );
    ensure!(app.paused, "language switching resumed a paused display");
    ensure!(
        app.message.as_ref() == Some(diagnostic),
        "cached typed diagnostic was discarded or rewritten"
    );
    let rendered = app.message.as_ref().unwrap().render();
    let expected = i18n::format_source_in(
        language,
        "操作失败：{error:#}",
        &[("error", RAW_DIAGNOSTIC)],
    )?;
    ensure!(
        rendered == expected && rendered.contains(RAW_DIAGNOSTIC),
        "localized feedback changed a raw Unicode diagnostic"
    );
    if language != Language::ZhCn {
        ensure!(
            !rendered.starts_with("操作失败："),
            "feedback still uses the Chinese source prefix"
        );
    }
    Ok(rendered)
}

fn check_fixture_settings(
    app: &SystemMonitorApp,
    expected: &Settings,
    language: Language,
) -> Result<()> {
    let mut expected = expected.clone();
    expected.language = language;
    ensure!(
        serde_json::to_value(&app.settings)? == serde_json::to_value(expected)?,
        "language switch changed unrelated saved preferences"
    );
    Ok(())
}

async fn wait_saved(settings_path: &Path, expected: &Settings, language: Language) -> Result<()> {
    let mut expected = serde_json::to_value(expected)?;
    expected["language"] = json!(language.tag());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let path = settings_path.to_owned();
        let saved = smol::unblock(move || {
            std::fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        })
        .await;
        if saved.as_ref() == Some(&expected) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "language {} and preferences were not saved to the isolated settings file",
            language.tag()
        );
        smol::Timer::after(SETTLE).await;
    }
}

async fn wait_hardware(
    view: &Entity<SystemMonitorApp>,
    hardware: &Arc<HardwareInventory>,
    cx: &mut AsyncApp,
) -> Result<u64> {
    smol::Timer::after(Duration::from_millis(400)).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (ready, generation) = cx.update(|cx| -> Result<_> {
            let app = view.read(cx);
            let current = app.hardware.as_ref().context("cached hardware inventory disappeared")?;
            ensure!(Arc::ptr_eq(current, hardware), "locale switch replaced the real hardware inventory instead of reformatting its cache");
            Ok((!app.hardware_sections.is_empty(), app.hardware_display_generation))
        })?;
        if ready {
            return Ok(generation);
        }
        ensure!(
            Instant::now() < deadline,
            "localized hardware presentation did not settle"
        );
        smol::Timer::after(SETTLE).await;
    }
}

async fn framework_defaults(
    handle: AnyWindowHandle,
    language: Language,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let mut input_keys = serde_json::Map::new();
    for key in [
        "Input.Cut",
        "Input.Copy",
        "Input.Paste",
        "Input.Select All",
        "Select.placeholder",
    ] {
        let actual = gpui_kit::component::_rust_i18n_try_translate(language.tag(), key)
            .with_context(|| format!("framework key {key} missing for {}", language.tag()))?;
        let expected = i18n::text_in(language, &format!("gpui_component.{key}"));
        ensure!(
            actual == expected && !actual.is_empty(),
            "framework default {key} differs from registered locale"
        );
        input_keys.insert(key.into(), json!(actual.as_ref()));
    }
    cx.update(|cx| {
        handle.update(cx, |_, window, cx| {
            window.open_alert_dialog(cx, |dialog, _, _| {
                dialog
                    .confirm()
                    .title("SystemMonitorX locale QA")
                    .description("Read-only default component translation check")
            });
        })
    })?;
    smol::Timer::after(SETTLE).await;
    let cancel = cx.update(|cx| {
        handle.update(cx, |_, window, cx| -> Result<String> {
            window.render_frame(cx);
            ensure!(
                window.has_active_dialog(cx),
                "framework default confirmation did not open"
            );
            let button = window
                .try_find("cancel")
                .context("framework default cancel button missing")?;
            ensure!(
                button.visible() && button.disabled() != Some(true),
                "framework cancel button unavailable"
            );
            let actual = button
                .label()
                .context("framework cancel button has no observed label")?
                .to_owned();
            ensure!(
                actual == i18n::text_in(language, "gpui_component.Dialog.cancel"),
                "visible default cancel label uses the wrong locale"
            );
            window.click("cancel", cx);
            Ok(actual)
        })
    })??;
    smol::Timer::after(SETTLE).await;
    cx.update(|cx| {
        handle.update(cx, |_, window, cx| {
            ensure!(
                !window.has_active_dialog(cx),
                "native default cancel click did not dismiss its harmless dialog"
            );
            Ok::<_, anyhow::Error>(())
        })
    })??;
    Ok(
        json!({"input_and_select_runtime_lookup": input_keys, "input_menu_native_click_tested": false,
        "input_lookup_scope": "exact translation function used by GPUI component defaults; no OS context-menu interaction",
        "dialog_cancel_native_clicked": true, "observed_cancel_label": cancel}),
    )
}

pub(super) async fn measure(
    handle: AnyWindowHandle,
    view: Entity<SystemMonitorApp>,
    directory: &Path,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let output = directory.join("locales");
    let create = output.clone();
    smol::unblock(move || std::fs::create_dir_all(create)).await?;
    let settings_path = Settings::path();
    ensure!(
        settings_path
            .parent()
            .context("QA settings parent missing")?
            .canonicalize()?
            == directory.canonicalize()?,
        "locale QA must use --ui-smoke-dir settings isolation"
    );
    let original = save_original(handle, &view, cx)?;
    let diagnostic = mf(
        "操作失败：{error:#}",
        &[("error", RAW_DIAGNOSTIC.to_owned())],
    );
    let owned_tcp = TcpListener::bind("127.0.0.1:0")?;
    let owned_udp = UdpSocket::bind("127.0.0.1:0")?;
    let tcp_endpoint = owned_tcp.local_addr()?;
    let udp_endpoint = owned_udp.local_addr()?;
    let query = format!("pid:{}", std::process::id());
    let executable = std::env::current_exe()?;
    let mut fixture_settings = original.settings.clone();
    fixture_settings
        .hidden_process_columns
        .retain(|column| *column != 1);
    fixture_settings.hidden_process_columns.extend([3, 5]);
    fixture_settings.hidden_process_columns.sort_unstable();
    fixture_settings.hidden_process_columns.dedup();
    let measured: Result<Value> = async {
        update_view(handle, &view, cx, |app, window, cx| {
            app.paused = true;
            app.settings = fixture_settings.clone();
            app.message = Some(diagnostic.clone());
            app.set_page(Page::Processes, window, cx);
            app.processes.update(cx, |table, cx| {
                let delegate = table.delegate_mut(); delegate.group_mode = false; delegate.tree_mode = false; delegate.detailed = false;
                delegate.set_hidden_columns(&fixture_settings.hidden_process_columns); delegate.sort_col = 0; delegate.descending = false; delegate.rebuild();
                let mut widths: Vec<_> = (0..table.delegate().columns_count(cx)).map(|index| table.delegate().column(index, cx).width).collect();
                widths[0] = px(341.); widths[1] = px(93.);
                table.delegate_mut().set_column_widths(&widths); table.refresh(cx); tables::sync_selection(table, cx); cx.notify();
            });
            window.resize(size(px(1040.), px(640.)));
            cx.notify(); Ok(())
        })?;
        replace_search(handle, &query, cx)?;
        smol::Timer::after(SETTLE).await;
        let identity = cx.update(|cx| -> Result<_> {
            let app = view.read(cx); let delegate = app.processes.read(cx).delegate();
            ensure!(delegate.rows.len() == 1, "owned PID native filter produced multiple or no rows");
            let process = delegate.row(0).context("owned process row missing")?;
            let ticks = process.start_time_windows_ticks.filter(|ticks| *ticks != 0).context("owned process lacks exact creation FILETIME")?;
            ensure!(process.pid == std::process::id(), "locale QA filter selected another process");
            Ok((process.pid, ticks))
        })?;
        let icon_deadline = Instant::now() + Duration::from_secs(15);
        let baseline_icon = loop {
            let icon = cx.update(|cx| view.read(cx).processes.read(cx).delegate().request_process_icon(&executable));
            if let Some(icon) = icon { break icon; }
            ensure!(Instant::now() < icon_deadline, "owned executable icon did not load for locale QA");
            smol::Timer::after(Duration::from_millis(100)).await;
        };
        ensure!(baseline_icon.source_path == executable && baseline_icon.width == 32 && baseline_icon.height == 32, "owned icon source/dimensions invalid");
        let mut samples = Vec::with_capacity(8);
        for language in Language::SUPPORTED {
            click(handle, "nav-settings", cx)?;
            smol::Timer::after(SETTLE).await;
            click(handle, format!("language-{}", language.tag()), cx)?;
            smol::Timer::after(SETTLE).await;
            cx.update(|cx| -> Result<()> { let app = view.read(cx); ensure!(app.page == Page::Settings, "native language click left Settings"); check_feedback(app, language, &diagnostic)?; check_fixture_settings(app, &fixture_settings, language) })?;
            wait_saved(&settings_path, &fixture_settings, language).await?;
            capture(handle, &output.join(format!("{}-settings.png", language.tag())), cx)?;
            let framework = framework_defaults(handle, language, cx).await?;

            click(handle, "nav-processes", cx)?;
            replace_search(handle, &query, cx)?;
            smol::Timer::after(SETTLE).await;
            let process_layout = click_process_checked(handle, &view, language, &output, cx)?;
            smol::Timer::after(SETTLE).await;
            let probe = if language == Language::EnUs { Language::ZhCn } else { Language::EnUs };
            for target in [probe, language] {
                update_view(handle, &view, cx, |app, window, cx| { app.set_language(target, window, cx); Ok(()) })?;
                smol::Timer::after(SETTLE).await;
                cx.update(|cx| -> Result<()> {
                    let app = view.read(cx); check_feedback(app, target, &diagnostic)?; check_fixture_settings(app, &fixture_settings, target)?;
                    ensure!(app.search.read(cx).value().as_ref() == query, "same-page locale switch cleared the input query");
                    let table = app.processes.read(cx); let delegate = table.delegate();
                    ensure!(delegate.query == query && delegate.rows.len() == 1 && delegate.selected == Some(identity), "same-page locale switch changed process query or exact selection");
                    ensure!(table.selected_row() == delegate.selected_index() && table.selected_row() == Some(0), "same-page switch left a stale native highlight");
                    ensure!(!delegate.group_mode && !delegate.tree_mode && !delegate.detailed, "same-page switch changed the process view mode");
                    Ok(())
                })?;
            }
            let process = cx.update(|cx| -> Result<Value> {
                let app = view.read(cx); let table = app.processes.read(cx); let delegate = table.delegate();
                let headers = table.headers(cx);
                ensure!(headers[0] == i18n::text_source_in(language, "进程名称") && headers[1] == "PID", "process headers use the wrong language");
                ensure!((0..delegate.columns_count(cx)).all(|index| !matches!(delegate.raw_column(index), Some(3 | 5))), "hidden process columns reappeared");
                ensure!(delegate.column(0, cx).width == px(341.) && delegate.column(1, cx).width == px(93.), "locale switch reset resized process widths");
                let icon = delegate.request_process_icon(&executable).context("owned icon vanished on locale change")?;
                ensure!(icon.source_path == executable && Arc::ptr_eq(&icon.image, &baseline_icon.image), "locale change replaced or misidentified the cached owned icon");
                Ok(json!({"headers":headers,"visible_rows":delegate.rows.len(),"exact_owned_pid_selection":true,"query_preserved":true,"paused":app.paused,
                    "hidden_preferences_preserved":true,"model_name_width_px":341,"model_pid_width_px":93,"owned_icon":{"width":icon.width,"height":icon.height,"same_cache_image":true}}))
            })?;
            cx.update(|cx| handle.update(cx, |_, window, cx| -> Result<()> {
                window.render_frame(cx);
                let name = window.try_find(("col-header",0usize)).context("native name header missing")?;
                let pid = window.try_find(("col-header",1usize)).context("native PID header missing")?;
                ensure!(name.visible() && pid.visible(), "native process headers are offscreen");
                ensure!((name.bounds().size.width.as_f32()-341.).abs()<1. && (pid.bounds().size.width.as_f32()-93.).abs()<1., "cached native header geometry lost resized widths");
                Ok(())
            }))??;
            capture(handle, &output.join(format!("{}-processes.png", language.tag())), cx)?;

            click(handle, "nav-ports", cx)?;
            replace_search(handle, &query, cx)?;
            let port_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let done = cx.update(|cx| !view.read(cx).ports_loading);
                if done { break; } ensure!(Instant::now() < port_deadline, "owned endpoint refresh did not settle"); smol::Timer::after(SETTLE).await;
            }
            let udp_row = cx.update(|cx| -> Result<usize> {
                let app=view.read(cx); let table=app.ports.read(cx); let delegate=table.delegate();
                ensure!(!delegate.rows.is_empty() && (0..delegate.rows.len()).all(|row| delegate.row(row).is_some_and(|entry| entry.process_identity()==Some(identity))), "localized port PID query changed owner identity");
                ensure!((0..delegate.rows.len()).any(|row| delegate.row(row).is_some_and(|entry| entry.protocol==PortProtocol::Tcp && entry.local==tcp_endpoint)), "owned TCP listener missing");
                (0..delegate.rows.len()).find(|row| delegate.row(*row).is_some_and(|entry| entry.protocol==PortProtocol::Udp && entry.local==udp_endpoint && entry.remote.is_none() && entry.tcp_state.is_none())).context("owned UDP binding missing or remote/state fabricated")
            })?;
            cx.update(|cx| { let table=view.read(cx).ports.clone(); table.update(cx,|table,cx|table.scroll_to_row(udp_row,cx)); });
            smol::Timer::after(SETTLE).await;
            click(handle, ("row",udp_row), cx)?;
            smol::Timer::after(SETTLE).await;
            let port_key = cx.update(|cx| view.read(cx).ports.read(cx).delegate().selected_entry().map(|entry|entry.key())).context("native UDP selection was not captured")?;
            for target in [probe, language] {
                update_view(handle, &view, cx, |app,window,cx| {app.set_language(target,window,cx);Ok(())})?;
                smol::Timer::after(SETTLE).await;
                cx.update(|cx| -> Result<()> {
                    let app=view.read(cx); check_feedback(app,target,&diagnostic)?;
                    let table=app.ports.read(cx); let delegate=table.delegate();
                    ensure!(app.search.read(cx).value().as_ref()==query && delegate.query==query, "same-page port locale switch cleared query");
                    ensure!(delegate.selected_entry().is_some_and(|entry|entry.key()==port_key && entry.process_identity()==Some(identity)), "same-page port locale switch changed captured owner/key");
                    ensure!(table.selected_row()==delegate.selected_index(), "same-page port locale switch left a stale highlight"); Ok(())
                })?;
            }
            let ports = cx.update(|cx| {let table=view.read(cx).ports.read(cx);json!({"headers":table.headers(cx),"visible_owned_endpoints":table.delegate().rows.len(),"tcp_and_udp_found":true,"pid_filetime_chain_preserved":true,"udp_remote_and_state_absent":true})});
            capture(handle, &output.join(format!("{}-ports.png",language.tag())), cx)?;

            click(handle, "nav-services", cx)?;
            cx.update(|cx| -> Result<()> {
                let app=view.read(cx);
                ensure!(app.page==Page::Services,"native nav-services click opened {} instead of Services in locale {}",app.page.source_title(),language.tag());
                Ok(())
            })?;
            let status_query = i18n::text_source_in(language,"运行中").into_owned();
            replace_search(handle,&status_query,cx)?;
            smol::Timer::after(SETTLE).await;
            let service_observation = cx.update(|cx| {
                let app = view.read(cx);
                let table = app.services.read(cx);
                let delegate = table.delegate();
                let input = app.search.read(cx).value().to_string();
                let codepoints = |text: &str| text.chars().map(|character|format!("U+{:04X}",character as u32)).collect::<Vec<_>>();
                let mut statuses = std::collections::BTreeMap::<String,usize>::new();
                for row in &delegate.all {
                    if let Some(status) = row.get(2) { *statuses.entry(status.clone()).or_default() += 1; }
                }
                let label_lengths = (0..5usize).map(|column|json!({
                    "column":column,
                    "max_raw_chars":delegate.all.iter().filter_map(|row|row.get(column)).map(|value|value.chars().count()).max().unwrap_or(0),
                    "max_current_display_chars":delegate.all.iter().filter_map(|row|row.get(column)).map(|value|{
                        if column==2 || column==4 {i18n::text_source(value).chars().count()} else {value.chars().count()}
                    }).max().unwrap_or(0)
                })).collect::<Vec<_>>();
                json!({
                    "locale":language.tag(),"resolved_locale":app.resolved_language.tag(),
                    "current_global_locale":rust_i18n::locale().to_string(),"current_i18n_language":i18n::current_language().tag(),
                    "current_page_source":app.page.source_title(),"is_services_page":app.page==Page::Services,"paused":app.paused,"metadata_loading":app.metadata_loading,
                    "search_input":input,"delegate_query":delegate.query,"expected_query":status_query,
                    "input_unicode_codepoints":codepoints(&input),"delegate_unicode_codepoints":codepoints(&delegate.query),
                    "expected_unicode_codepoints":codepoints(&status_query),
                    "all_row_count":delegate.all.len(),"visible_row_count":delegate.rows.len(),
                    "raw_running_count":delegate.all.iter().filter(|row|row.get(2).is_some_and(|status|status=="运行中")).count(),
                    "os_service_entry_count":app.service_entries.len(),
                    "os_running_count":app.service_entries.iter().filter(|service|service.state==system_monitor_x::platform::ServiceState::Running).count(),
                    "all_status_counts":statuses,"first_all_rows":delegate.all.iter().take(6).cloned().collect::<Vec<_>>(),
                    "first_visible_rows":delegate.rows.iter().take(6).cloned().collect::<Vec<_>>(),
                    "translated_running_current":i18n::text_source("运行中"),
                    "translated_running_explicit":i18n::text_source_in(language,"运行中"),
                    "headers":table.headers(cx),"max_service_label_lengths":label_lengths,
                    "native_wait_ms":SETTLE.as_millis(),"search_scope":"visible columns only; statuses/start type localized, no hidden identities"
                })
            });
            let services = cx.update(|cx| -> Result<Value> {
                let app=view.read(cx); let table=app.services.read(cx);let delegate=table.delegate();
                ensure!(delegate.query==status_query && !delegate.rows.is_empty(), "native service query cannot find its translated status");
                ensure!(delegate.rows.iter().any(|row|row.get(2).is_some_and(|state|state=="运行中")), "translated service query found no known running service");
                let headers=table.headers(cx);
                for (index,source) in ["服务名称","显示名称","状态","PID","启动类型"].into_iter().enumerate(){ensure!(headers[index]==i18n::text_source_in(language,source),"service header {source} not localized");}
                ensure!(table.selected_row()==delegate.selected_row(), "service filtering left stale native selection");
                Ok(json!({"headers":headers,"visible_matches":delegate.rows.len(),"known_status_translation":status_query,"native_translated_status_query":true}))
            });
            let services = match services {
                Ok(services) => services,
                Err(error) => {
                    let screenshot = output.join(format!("{}-services-query-failure.png",language.tag()));
                    let json_path = output.join(format!("{}-services-query-failure.json",language.tag()));
                    let navigation_layout = cx.update(|cx| handle.update(cx,|_,window,cx| {
                        window.render_frame(cx);
                        let viewport = window.viewport_size();
                        let parent = window.try_find("sidebar-nav").map(|element|element.bounds());
                        let mut elements = serde_json::Map::new();
                        for id in ["nav-services","sidebar-nav","nav-settings","sidebar-footer"] {
                            let element = window.try_find(id);
                            let mut facts = element_layout(element.clone(),viewport);
                            facts["center_inside_sidebar_nav"] = element.as_ref().and_then(|element|parent.map(|parent| {
                                let center = element.bounds().center();
                                center.x >= parent.left() && center.x <= parent.right()
                                    && center.y >= parent.top() && center.y <= parent.bottom()
                            })).map(Value::Bool).unwrap_or(Value::Null);
                            elements.insert(id.to_owned(),facts);
                        }
                        json!({"viewport":{"width":viewport.width.as_f32(),"height":viewport.height.as_f32()},"elements":elements})
                    })).ok();
                    let screenshot_error = capture(handle,&screenshot,cx).err().map(|error|format!("{error:#}"));
                    let failure = json!({"passed":false,"error":format!("{error:#}"),"observation":service_observation,"navigation_layout":navigation_layout,"screenshot_error":screenshot_error});
                    let write_error = serde_json::to_vec_pretty(&failure).map_err(anyhow::Error::from)
                        .and_then(|bytes|std::fs::write(&json_path,bytes).map_err(anyhow::Error::from))
                        .err().map(|error|format!("{error:#}"));
                    return Err(error).context(format!("locale {} service query failed; observation={service_observation}; navigation={navigation_layout:?}; failure PNG={} (capture error={screenshot_error:?}); JSON={} (write error={write_error:?})",
                        language.tag(),screenshot.display(),json_path.display()));
                }
            };
            capture(handle,&output.join(format!("{}-services.png",language.tag())),cx)?;

            click(handle,"nav-devices",cx)?;
            let hardware_generation=wait_hardware(&view,&original.hardware,cx).await?;
            cx.update(|cx| -> Result<()> {let app=view.read(cx);ensure!(app.page==Page::Devices && app.resolved_language==language,"devices page did not retain locale");check_feedback(app,language,&diagnostic)?;check_fixture_settings(app,&fixture_settings,language)?;Ok(())})?;
            capture(handle,&output.join(format!("{}-devices.png",language.tag())),cx)?;
            samples.push(json!({"locale":language.tag(),"native_settings_button_clicked":true,"isolated_settings_saved":true,"process":process,"process_click_layout":process_layout,"ports":ports,"services":services,
                "cached_typed_feedback_retranslated":true,"raw_unicode_fixture_unchanged":true,"diagnostic_fixture_is_os_telemetry":false,"framework":framework,
                "hardware":{"same_inventory_arc":true,"raw_model_data_unchanged":true,"presentation_generation":hardware_generation,"minimum_settle_ms":400},
                "screenshots":[format!("locales/{}-settings.png",language.tag()),format!("locales/{}-processes.png",language.tag()),format!("locales/{}-ports.png",language.tag()),format!("locales/{}-services.png",language.tag()),format!("locales/{}-devices.png",language.tag())]}));
        }
        Ok(json!({"passed":true,"locale_count":samples.len(),"native_settings_click_count":samples.len(),"screenshot_count":samples.len()*5,
            "scope":"Native GPUI event dispatch and rendering with real Windows process, endpoint, service and cached hardware data; only feedback text is an explicit QA fixture",
            "screenshots_are_private":true,"glyphs_manually_reviewed":false,"ui_telemetry_injection":false,"samples":samples}))
    }.await;
    let cleanup = restore(handle, &view, &original, cx);
    smol::Timer::after(SETTLE).await;
    match (measured, cleanup) {
        (Ok(mut value), Ok(())) => {
            cx.update(|cx| -> Result<()> {
                let app = view.read(cx);
                ensure!(
                    app.page == original.page
                        && app.paused == original.paused
                        && app.search.read(cx).value().as_ref() == original.search,
                    "locale QA did not restore page/pause/search"
                );
                ensure!(
                    app.message == original.message,
                    "locale QA did not restore original cached feedback"
                );
                ensure!(
                    serde_json::to_value(&app.settings)?
                        == serde_json::to_value(&original.settings)?,
                    "locale QA did not restore saved preferences"
                );
                let table = app.processes.read(cx);
                ensure!(
                    table.selected_row() == table.delegate().selected_index(),
                    "locale QA cleanup left stale process highlight"
                );
                Ok(())
            })?;
            wait_saved(
                &settings_path,
                &original.settings,
                original.settings.language,
            )
            .await?;
            value["original_settings_page_pause_query_feedback_restored"] = json!(true);
            Ok(value)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error).context("restore locale QA state"),
        (Err(error), Err(cleanup)) => {
            Err(error).context(format!("locale QA cleanup also failed: {cleanup:#}"))
        }
    }
}
