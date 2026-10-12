//! Opt-in native UI timing. The live samples dispatch through GPUI's native
//! keyboard path and render the real Windows process table. Timers measure
//! CPU-side wall-clock phases, never PNG work or GPU presentation latency.
//! The separate 10,000-row model benchmark is explicitly synthetic and is
//! never installed in the application's telemetry snapshot.
use super::{Page, ProcessTable, SystemMonitorApp};
use anyhow::{Context as _, Result, ensure};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, App, AsyncApp, Entity, InputEvent as _, KeyUpEvent, Keystroke, Window,
};
use serde_json::{Value, json};
use std::{
    cmp::Ordering,
    sync::Arc,
    time::{Duration, Instant},
};
use system_monitor_x::monitor::{ProcessSample, SystemSnapshot};

const QUERIES: [&str; 3] = ["SystemMonitorX", "sys", ""];
const LIVE_ITERATIONS: usize = 30;
const SYNTHETIC_ROWS: usize = 10_000;

/// Measure one complete native key-down/key-up dispatch directly. Avoid the
/// test helper's extra before/after frames, but preserve GPUI's own synchronous
/// draw when a dirty view must be refreshed before dispatching a key event.
/// Queued InputEvent/filter effect flushing remains part of the batch phase.
fn press(window: &mut Window, key: Keystroke, cx: &mut App) -> f64 {
    let start = Instant::now();
    window.dispatch_keystroke(key.clone(), cx);
    window.dispatch_event(KeyUpEvent { keystroke: key }.to_platform_input(), cx);
    milliseconds(start)
}

fn replace_search_with_press_times(
    window: &mut Window,
    query: &str,
    cx: &mut App,
) -> Result<Vec<f64>> {
    let mut press_times = Vec::with_capacity(query.chars().count() + 2);
    press_times.push(press(window, Keystroke::parse("ctrl-a")?, cx));
    press_times.push(press(window, Keystroke::parse("backspace")?, cx));
    for character in query.chars() {
        let text = character.to_string();
        let mut key = Keystroke::parse(&text)?;
        key.key_char = Some(text);
        press_times.push(press(window, key, cx));
    }
    Ok(press_times)
}

fn replace_search(window: &mut Window, query: &str, cx: &mut App) -> Result<()> {
    replace_search_with_press_times(window, query, cx).map(|_| ())
}

fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1_000.0
}

/// Nearest-rank percentiles: p95 is the 29th sorted value for 30 samples.
fn statistics(values: &[f64]) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort_unstable_by(f64::total_cmp);
    if sorted.is_empty() {
        return json!({"samples": 0, "p50_ms": null, "p95_ms": null, "max_ms": null});
    }
    let percentile = |fraction: f64| {
        let rank = (fraction * sorted.len() as f64).ceil() as usize;
        sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
    };
    json!({
        "samples": sorted.len(),
        "percentile_method": "nearest_rank",
        "p50_ms": percentile(0.50),
        "p95_ms": percentile(0.95),
        "max_ms": sorted[sorted.len() - 1],
    })
}

fn matches_query(process: &ProcessSample, query: &str) -> bool {
    query.is_empty()
        || format!(
            "{}\n{}\n{}\n{}",
            process.name,
            process.pid,
            process
                .exe
                .as_ref()
                .map(|path| path.to_string_lossy())
                .unwrap_or_default(),
            process.user_name.as_deref().unwrap_or("")
        )
        .to_lowercase()
        .contains(query)
}

fn validate_live(
    view: &Entity<SystemMonitorApp>,
    query: &str,
    cx: &App,
) -> Result<(usize, usize, u64, bool)> {
    let app = view.read(cx);
    ensure!(
        app.page == Page::Processes,
        "live timing left the process page"
    );
    ensure!(
        app.search.read(cx).value().as_ref() == query,
        "native search input did not equal {query:?}"
    );
    let snapshot = app
        .snapshot
        .as_ref()
        .context("live process snapshot unavailable")?;
    ensure!(snapshot.cpu_ready, "live CPU sample not ready");
    ensure!(
        !snapshot.processes.is_empty(),
        "live Windows process snapshot empty"
    );
    let table = app.processes.read(cx).delegate();
    ensure!(
        table.query == query,
        "native input did not update process filter"
    );
    ensure!(
        !table.rows.is_empty(),
        "live query {query:?} produced no rows"
    );
    let folded = query.to_lowercase();
    let mut expected: Vec<_> = snapshot
        .processes
        .iter()
        .filter(|process| matches_query(process, &folded))
        .map(super::tables::process_identity)
        .collect();
    let mut visible: Vec<_> = (0..table.rows.len())
        .map(|index| {
            table
                .row(index)
                .map(super::tables::process_identity)
                .context("visible row has no live process")
        })
        .collect::<Result<_>>()?;
    expected.sort_unstable();
    visible.sort_unstable();
    ensure!(
        expected == visible,
        "visible live process identities differ from expected filter for {query:?}"
    );
    Ok((
        snapshot.processes.len(),
        table.rows.len(),
        table.name_sort_count(),
        table.name_order_reused(),
    ))
}

async fn live_measurement(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    cx.update(|cx| {
        handle.update(cx, |_, window, cx| {
            window.click("nav-进程", cx);
            window.press("ctrl-f", cx);
            replace_search(window, "", cx)
        })
    })??;
    smol::Timer::after(Duration::from_millis(16)).await;
    cx.update(|cx| handle.update(cx, |_, window, cx| window.render_frame(cx)))?;

    let mut samples = Vec::with_capacity(LIVE_ITERATIONS);
    let mut dispatch_times = Vec::with_capacity(LIVE_ITERATIONS);
    let mut render_times = Vec::with_capacity(LIVE_ITERATIONS);
    let mut combined_times = Vec::with_capacity(LIVE_ITERATIONS);
    let mut process_counts = Vec::with_capacity(LIVE_ITERATIONS);
    let mut measured_press_times = Vec::new();
    let mut query_dispatch_times: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
    let mut query_render_times: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
    let mut query_combined_times: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
    let mut query_press_times: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
    for index in 0..LIVE_ITERATIONS {
        let query = QUERIES[index % QUERIES.len()];
        // This is a complete replacement batch: clear plus every character.
        // GPUI can synchronously draw dirty views inside key-down/key-up, and
        // AsyncApp::update flushes InputEvent/filter effects before returning.
        // All such work belongs to this batch phase, not just the explicit
        // render_frame phase below. Per-press timings are measured directly,
        // never estimated by dividing the batch duration by character count.
        let start = Instant::now();
        let press_times = cx.update(|cx| {
            handle.update(cx, |_, window, cx| {
                replace_search_with_press_times(window, query, cx)
            })
        })??;
        let dispatch_ms = milliseconds(start);
        let start = Instant::now();
        let dimensions = cx.update(|cx| {
            handle.update(cx, |_, window, cx| {
                window.render_frame(cx);
                let viewport = window.viewport_size();
                (
                    f64::from(viewport.width),
                    f64::from(viewport.height),
                    f64::from(window.scale_factor()),
                )
            })
        })?;
        let render_ms = milliseconds(start);
        // Identity validation is intentionally outside both timed phases.
        let (process_count, visible_count, name_sort_count, name_order_reused) =
            cx.update(|cx| validate_live(view, query, cx))?;
        dispatch_times.push(dispatch_ms);
        render_times.push(render_ms);
        combined_times.push(dispatch_ms + render_ms);
        process_counts.push(process_count);
        let query_index = index % QUERIES.len();
        query_dispatch_times[query_index].push(dispatch_ms);
        query_render_times[query_index].push(render_ms);
        query_combined_times[query_index].push(dispatch_ms + render_ms);
        query_press_times[query_index].extend_from_slice(&press_times);
        measured_press_times.extend_from_slice(&press_times);
        samples.push(json!({
            "iteration": index + 1,
            "query": query,
            "native_key_count": press_times.len(),
            "keydown_up_count": press_times.len() * 2,
            "dispatch_includes_dirty_draws": true,
            "test_support_instrumentation": true,
            "measured_per_key_press": statistics(&press_times),
            "key_press_cpu_phase_ms": press_times,
            "real_process_count": process_count,
            "visible_process_count": visible_count,
            "name_sort_count": name_sort_count,
            "name_order_reused_from_previous_snapshot": name_order_reused,
            "keyboard_dispatch_and_filter_ms": dispatch_ms,
            "render_frame_cpu_phase_ms": render_ms,
            "combined_cpu_phase_ms": dispatch_ms + render_ms,
            "window": {"logical_width_px": dimensions.0, "logical_height_px": dimensions.1, "scale_factor": dimensions.2},
            "filter_identity_validation_passed": true,
        }));
        // Let the native event loop, live collector ingestion and renderer run
        // between samples. The wait is excluded from all reported durations.
        smol::Timer::after(Duration::from_millis(16)).await;
    }
    let per_query: Vec<_> = QUERIES
        .iter()
        .enumerate()
        .map(|(index, query)| {
            json!({
                "query": query,
                "replacement_character_count": query.chars().count(),
                "native_key_count": query_press_times[index].len(),
                "keydown_up_count": query_press_times[index].len() * 2,
                "dispatch_includes_dirty_draws": true,
                "test_support_instrumentation": true,
                "keyboard_dispatch_and_filter": statistics(&query_dispatch_times[index]),
                "render_frame_cpu_phase": statistics(&query_render_times[index]),
                "combined_cpu_phase": statistics(&query_combined_times[index]),
                "measured_per_key_press": statistics(&query_press_times[index]),
            })
        })
        .collect();
    Ok(json!({
        "data_source": "real_windows_process_snapshots",
        "measurement": "CPU-side wall-clock full-query keyboard replacement batches, directly measured key presses, and explicit render_frame phases",
        "native_key_count": measured_press_times.len(),
        "keydown_up_count": measured_press_times.len() * 2,
        "dispatch_includes_dirty_draws": true,
        "test_support_instrumentation": true,
        "batch_is_single_key_latency": false,
        "per_key_measurement_scope": "Direct dispatch_keystroke plus key-up duration, including synchronous dirty draws; queued filter/effect flush is included in batch timing",
        "measured_per_key_press": statistics(&measured_press_times),
        "per_query": per_query,
        "is_gpu_presentation_latency": false,
        "includes_png_capture": false,
        "inter_sample_wait_ms": 16,
        "real_process_count_min": process_counts.iter().min(),
        "real_process_count_max": process_counts.iter().max(),
        "keyboard_dispatch_and_filter": statistics(&dispatch_times),
        "render_frame_cpu_phase": statistics(&render_times),
        "combined_cpu_phase": statistics(&combined_times),
        "samples": samples,
    }))
}

fn synthetic_process(index: usize) -> ProcessSample {
    let family = match index % 4 {
        0 | 1 => "SystemMonitorX-Synthetic",
        2 => "sys-synthetic",
        _ => "synthetic-worker",
    };
    ProcessSample {
        pid: 100_000 + index as u32,
        name: format!("{family}-{index:05}"),
        cpu_usage: (index % 997) as f32 * 0.1,
        memory_bytes: 32 * 1024 * 1024 + (index % 1000) as u64 * 8192,
        start_time_windows_ticks: Some(133_000_000_000_000_000 + index as u64),
        status: "synthetic model benchmark".into(),
        ..ProcessSample::default()
    }
}

fn model_order(a: &ProcessSample, b: &ProcessSample, column: usize, descending: bool) -> Ordering {
    let primary = match column {
        0 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        1 => a.pid.cmp(&b.pid),
        2 => a.cpu_usage.total_cmp(&b.cpu_usage),
        3 => a.memory_bytes.cmp(&b.memory_bytes),
        _ => unreachable!("benchmark uses only name/PID/CPU/memory sorting"),
    };
    let primary = if descending {
        primary.reverse()
    } else {
        primary
    };
    primary.then(a.pid.cmp(&b.pid))
}

fn synthetic_measurement() -> Result<Value> {
    let start = Instant::now();
    let snapshot = Arc::new(SystemSnapshot {
        cpu_ready: true,
        processes: (0..SYNTHETIC_ROWS).map(synthetic_process).collect(),
        ..SystemSnapshot::default()
    });
    let construction_ms = milliseconds(start);
    let mut table = ProcessTable::new();
    let start = Instant::now();
    table.set_snapshot(snapshot);
    let snapshot_index_and_initial_sort_ms = milliseconds(start);
    let mut durations = Vec::with_capacity(30);
    let mut samples = Vec::with_capacity(30);
    for index in 0..30 {
        let query = QUERIES[index % QUERIES.len()];
        let column = index % 4;
        let descending = index % 2 == 0;
        table.query = query.into();
        table.sort_col = column;
        table.descending = descending;
        let start = Instant::now();
        table.rebuild();
        let elapsed_ms = milliseconds(start);
        let expected_rows = match query {
            "SystemMonitorX" => SYNTHETIC_ROWS / 2,
            "sys" => SYNTHETIC_ROWS * 3 / 4,
            "" => SYNTHETIC_ROWS,
            _ => unreachable!(),
        };
        ensure!(
            table.rows.len() == expected_rows,
            "synthetic query {query:?} has unexpected row count"
        );
        let folded = query.to_lowercase();
        ensure!(
            (0..table.rows.len()).all(|row| table
                .row(row)
                .is_some_and(|process| matches_query(process, &folded))),
            "synthetic filter returned an unrelated row"
        );
        ensure!(
            (1..table.rows.len()).all(|row| model_order(
                table.row(row - 1).unwrap(),
                table.row(row).unwrap(),
                column,
                descending
            ) != Ordering::Greater),
            "synthetic visible rows are not sorted by the requested column"
        );
        durations.push(elapsed_ms);
        samples.push(json!({"iteration": index + 1, "query": query, "sort_column": column,
            "descending": descending, "visible_rows": table.rows.len(), "filter_sort_ms": elapsed_ms,
            "filter_and_sort_validation_passed": true}));
    }
    Ok(json!({
        "data_source": "synthetic_process_rows_not_os_telemetry",
        "model": "ProcessTable with immutable Arc<SystemSnapshot> and cached search keys",
        "model_process_count": SYNTHETIC_ROWS,
        "runs_on_background_worker": true,
        "includes_native_render": false,
        "injected_into_live_ui": false,
        "synthetic_row_construction_ms": construction_ms,
        "snapshot_index_and_initial_sort_ms": snapshot_index_and_initial_sort_ms,
        "filter_and_sort": statistics(&durations),
        "samples": samples,
    }))
}

fn restore(
    handle: AnyWindowHandle,
    view: &Entity<SystemMonitorApp>,
    original_page: Page,
    cx: &mut AsyncApp,
) -> Result<()> {
    cx.update(|cx| {
        handle.update(cx, |_, window, cx| {
            window.press("ctrl-f", cx);
            replace_search(window, "", cx)
        })
    })??;
    cx.update(|cx| handle.update(cx, |_, window, cx| window.render_frame(cx)))?;
    cx.update(|cx| -> Result<()> {
        ensure!(
            view.read(cx).search.read(cx).value().is_empty(),
            "search was not cleared after native timing"
        );
        ensure!(
            view.read(cx).processes.read(cx).delegate().query.is_empty(),
            "model search was not cleared after native timing"
        );
        if original_page != Page::Processes {
            handle.update(cx, |_, window, cx| {
                window.click(format!("nav-{}", original_page.title()), cx)
            })?;
        }
        ensure!(
            view.read(cx).page == original_page,
            "native timing did not restore original page"
        );
        Ok(())
    })
}

pub(super) async fn measure(
    handle: AnyWindowHandle,
    view: Entity<SystemMonitorApp>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let original_page = cx.update(|cx| view.read(cx).page);
    let original_modes = cx.update(|cx| {
        let table = view.read(cx).processes.clone();
        table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            let modes = (delegate.group_mode, delegate.tree_mode);
            delegate.group_mode = false;
            delegate.tree_mode = false;
            delegate.rebuild();
            super::tables::sync_selection(table, cx);
            cx.notify();
            modes
        })
    });
    let measured: Result<Value> = async {
        let live = live_measurement(handle, &view, cx).await?;
        let synthetic = smol::unblock(synthetic_measurement).await?;
        Ok(json!({"native_live": live, "synthetic_model_10000": synthetic}))
    }
    .await;
    // Also clear the search when a validation fails. Keeping cleanup outside
    // the measured future prevents `?` from leaving a benchmark query behind.
    let cleanup = restore(handle, &view, original_page, cx);
    cx.update(|cx| {
        let table = view.read(cx).processes.clone();
        table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.group_mode = original_modes.0;
            delegate.tree_mode = original_modes.1;
            delegate.rebuild();
            super::tables::sync_selection(table, cx);
            cx.notify();
        });
    });
    match (measured, cleanup) {
        (Ok(mut result), Ok(())) => {
            cx.update(|cx| -> Result<()> {
                let table = view.read(cx).processes.read(cx);
                ensure!(
                    (table.delegate().group_mode, table.delegate().tree_mode) == original_modes,
                    "performance view modes were not restored"
                );
                ensure!(
                    table.selected_row() == table.delegate().selected_index(),
                    "performance cleanup left stale native row selection"
                );
                Ok(())
            })?;
            result["search_cleared_and_original_page_restored"] = json!(true);
            result["process_view_mode"] =
                json!("flat; original grouping/tree modes restored after measurement");
            Ok(result)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup.context("native performance cleanup")),
        (Err(error), Err(cleanup)) => Err(error.context(format!(
            "native performance cleanup also failed: {cleanup:#}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{statistics, synthetic_measurement};

    #[test]
    fn percentiles_use_nearest_rank_and_preserve_outliers() {
        let mut values: Vec<_> = (1..=30).map(f64::from).collect();
        values.reverse();
        let stats = statistics(&values);
        assert_eq!(stats["p50_ms"], 15.0);
        assert_eq!(stats["p95_ms"], 29.0);
        assert_eq!(stats["max_ms"], 30.0);
        assert!(statistics(&[])["p95_ms"].is_null());
    }

    #[test]
    fn ten_thousand_row_model_validates_filters_and_sorting() {
        let report = synthetic_measurement().unwrap();
        assert_eq!(report["model_process_count"], 10_000);
        assert_eq!(report["filter_and_sort"]["samples"], 30);
        assert_eq!(report["samples"][0]["visible_rows"], 5_000);
        assert_eq!(report["samples"][1]["visible_rows"], 7_500);
        assert_eq!(report["samples"][2]["visible_rows"], 10_000);
        assert_eq!(report["injected_into_live_ui"], false);
    }
}
