//! CSV output uses RFC 4180 quoting. Opening a spreadsheet must not turn a
//! process name, account, status or executable path into a formula.
use crate::monitor::{ProcessSample, SystemSnapshot};
use anyhow::Result;
use std::{io::Write, path::Path};

fn csv_cell(value: &str) -> String {
    let first = value
        .trim_start_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
        .chars()
        .next();
    let guarded =
        if matches!(first, Some('=' | '+' | '-' | '@')) || value.starts_with(['\t', '\r', '\n']) {
            format!("'{value}")
        } else {
            value.to_owned()
        };
    format!("\"{}\"", guarded.replace('"', "\"\""))
}

// Keep the original eleven columns in order for existing CSV consumers. New
// column names distinguish committed memory, resident memory and I/O counters.
const COLUMNS: [&str; 28] = [
    "Name",
    "PID",
    "Parent PID",
    "CPU %",
    "Memory bytes",
    "GPU %",
    "GPU memory bytes",
    "Read bytes/s",
    "Write bytes/s",
    "Start time (Unix seconds)",
    "Executable",
    "Virtual memory bytes",
    "Working set bytes",
    "Private committed bytes",
    "Private working set bytes",
    "CPU time seconds",
    "CPU time (100 ns ticks)",
    "Threads",
    "Handles",
    "Session ID",
    "User name",
    "Total I/O read bytes",
    "Total I/O write bytes",
    "Network received bytes/s",
    "Network transmitted bytes/s",
    "Run time seconds",
    "Start time (Windows FILETIME ticks)",
    "Status",
];

fn optional_integer(value: Option<impl ToString>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn decimal(value: Option<f64>, precision: usize) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{value:.precision$}"))
        .unwrap_or_default()
}

fn process_row(process: &ProcessSample, cpu_ready: bool) -> [String; COLUMNS.len()] {
    let executable = process.exe.as_ref().map(|path| path.to_string_lossy());
    let start_time = (process.start_time_unix_secs > 0).then_some(process.start_time_unix_secs);
    [
        csv_cell(&process.name),
        process.pid.to_string(),
        optional_integer(process.parent_pid),
        decimal(cpu_ready.then_some(process.cpu_usage as f64), 2),
        process.memory_bytes.to_string(),
        decimal(process.gpu_usage.map(f64::from), 2),
        optional_integer(process.gpu_memory_bytes),
        decimal(process.disk_read_bytes_per_sec, 2),
        decimal(process.disk_write_bytes_per_sec, 2),
        optional_integer(start_time),
        csv_cell(executable.as_deref().unwrap_or("")),
        process.virtual_memory_bytes.to_string(),
        optional_integer(process.working_set_bytes),
        optional_integer(process.private_bytes),
        optional_integer(process.private_working_set_bytes),
        // Native CPU time is measured in 100 ns; preserve that precision and
        // export original integer ticks separately without a float conversion.
        decimal(process.cpu_time_seconds, 7),
        optional_integer(process.cpu_time_ticks),
        optional_integer(process.thread_count),
        optional_integer(process.handle_count),
        optional_integer(process.session_id),
        csv_cell(process.user_name.as_deref().unwrap_or("")),
        optional_integer(process.total_disk_read_bytes),
        optional_integer(process.total_disk_write_bytes),
        decimal(process.network_received_bytes_per_sec, 2),
        decimal(process.network_transmitted_bytes_per_sec, 2),
        optional_integer(start_time.map(|_| process.run_time_secs)),
        optional_integer(process.start_time_windows_ticks.filter(|ticks| *ticks > 0)),
        csv_cell(&process.status),
    ]
}

fn write_csv(snapshot: &SystemSnapshot, output: &mut impl Write) -> Result<()> {
    output.write_all(b"\xef\xbb\xbf")?;
    output.write_all(COLUMNS.join(",").as_bytes())?;
    output.write_all(b"\r\n")?;
    for process in &snapshot.processes {
        output.write_all(
            process_row(process, snapshot.cpu_ready)
                .join(",")
                .as_bytes(),
        )?;
        output.write_all(b"\r\n")?;
    }
    output.flush()?;
    Ok(())
}

pub fn write_process_csv(snapshot: &SystemSnapshot, path: &Path) -> Result<()> {
    write_csv(snapshot, &mut std::fs::File::create(path)?)
}

#[cfg(test)]
mod tests {
    use super::{COLUMNS, csv_cell, process_row, write_csv};
    use crate::monitor::{ProcessSample, SystemSnapshot};

    fn field<'a>(row: &'a [String], column: &str) -> &'a str {
        &row[COLUMNS
            .iter()
            .position(|name| *name == column)
            .expect("known column")]
    }

    #[test]
    fn escapes_quotes_and_formula_prefixes() {
        assert_eq!(csv_cell("one,\"two\""), "\"one,\"\"two\"\"\"");
        assert_eq!(
            csv_cell(" =HYPERLINK(\"x\")"),
            "\"' =HYPERLINK(\"\"x\"\")\""
        );
        assert_eq!(csv_cell("app.exe"), "\"app.exe\"");
        assert_eq!(csv_cell("a\r\nb"), "\"a\r\nb\"");
        for input in [
            "+cmd",
            "-cmd",
            "@cmd",
            "\tplain",
            "\rplain",
            "\nplain",
            "\u{feff}  =cmd",
        ] {
            assert!(csv_cell(input).starts_with("\"'"));
        }
    }

    #[test]
    fn missing_metrics_stay_empty_and_measured_zero_is_preserved() {
        let process = ProcessSample {
            pid: 23,
            name: "fixture.exe".into(),
            start_time_unix_secs: 1_700_000_000,
            gpu_usage: Some(0.),
            thread_count: Some(0),
            private_bytes: Some(0),
            cpu_time_ticks: Some(0),
            network_received_bytes_per_sec: Some(0.),
            network_transmitted_bytes_per_sec: Some(f64::NAN),
            ..Default::default()
        };
        let row = process_row(&process, false);
        for column in [
            "CPU %",
            "Parent PID",
            "Working set bytes",
            "Private working set bytes",
            "CPU time seconds",
            "Handles",
            "Session ID",
            "Total I/O read bytes",
            "Total I/O write bytes",
            "Read bytes/s",
            "Write bytes/s",
            "Network transmitted bytes/s",
            "Start time (Windows FILETIME ticks)",
        ] {
            assert_eq!(
                field(&row, column),
                "",
                "missing {column} must remain empty"
            );
        }
        assert_eq!(field(&row, "User name"), "\"\"");
        assert_eq!(field(&row, "GPU %"), "0.00");
        assert_eq!(field(&row, "Threads"), "0");
        assert_eq!(field(&row, "Private committed bytes"), "0");
        assert_eq!(field(&row, "CPU time (100 ns ticks)"), "0");
        assert_eq!(field(&row, "Network received bytes/s"), "0.00");
        assert_eq!(field(&row, "Run time seconds"), "0");
        let unknown_identity = process_row(&ProcessSample::default(), true);
        assert_eq!(field(&unknown_identity, "Start time (Unix seconds)"), "");
        assert_eq!(field(&unknown_identity, "Run time seconds"), "");
    }

    #[test]
    fn csv_contains_full_precision_identity_and_protects_all_text_fields() {
        let process = ProcessSample {
            pid: 42,
            name: "=app,\"name\"\r\nnext".into(),
            user_name: Some("@user".into()),
            exe: Some("+fixture.exe".into()),
            status: "-status".into(),
            start_time_unix_secs: 1_700_000_000,
            start_time_windows_ticks: Some(133_444_736_123_456_789),
            cpu_time_seconds: Some(0.0000001),
            cpu_time_ticks: Some(1),
            memory_bytes: 101,
            virtual_memory_bytes: 202,
            working_set_bytes: Some(303),
            private_bytes: Some(404),
            private_working_set_bytes: Some(505),
            total_disk_read_bytes: Some(606),
            total_disk_write_bytes: Some(707),
            ..Default::default()
        };
        let row = process_row(&process, true);
        assert_eq!(
            field(&row, "Start time (Windows FILETIME ticks)"),
            "133444736123456789"
        );
        assert_eq!(field(&row, "CPU time seconds"), "0.0000001");
        assert_eq!(field(&row, "CPU time (100 ns ticks)"), "1");
        assert_eq!(field(&row, "Memory bytes"), "101");
        assert_eq!(field(&row, "Virtual memory bytes"), "202");
        assert_eq!(field(&row, "Working set bytes"), "303");
        assert_eq!(field(&row, "Private committed bytes"), "404");
        assert_eq!(field(&row, "Private working set bytes"), "505");
        assert_eq!(field(&row, "Total I/O read bytes"), "606");
        assert_eq!(field(&row, "Total I/O write bytes"), "707");
        assert_eq!(field(&row, "User name"), "\"'@user\"");
        assert_eq!(field(&row, "Executable"), "\"'+fixture.exe\"");
        assert_eq!(field(&row, "Status"), "\"'-status\"");
        let snapshot = SystemSnapshot {
            cpu_ready: true,
            processes: vec![process],
            ..Default::default()
        };
        let mut output = Vec::new();
        write_csv(&snapshot, &mut output).unwrap();
        assert!(output.starts_with(b"\xef\xbb\xbfName,PID,Parent PID"));
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("\r\n\"'=app,\"\"name\"\"\r\nnext\",42,"));
        assert!(text.ends_with("\"'-status\"\r\n"));
    }
}
