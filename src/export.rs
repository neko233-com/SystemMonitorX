//! CSV output uses RFC 4180 quoting. Opening a spreadsheet must not turn a
//! process name or executable path into a formula.
use crate::monitor::SystemSnapshot;
use anyhow::Result;
use std::{io::Write, path::Path};

fn csv_cell(value: &str) -> String {
    let first = value.trim_start().chars().next();
    let guarded = if matches!(first, Some('=' | '+' | '-' | '@' | '\t' | '\r')) {
        format!("'{value}")
    } else {
        value.to_owned()
    };
    format!("\"{}\"", guarded.replace('"', "\"\""))
}

pub fn write_process_csv(snapshot: &SystemSnapshot, path: &Path) -> Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(b"\xef\xbb\xbf")?;
    writeln!(
        file,
        "Name,PID,Parent PID,CPU %,Memory bytes,GPU %,GPU memory bytes,Read bytes/s,Write bytes/s,Start time (Unix seconds),Executable"
    )?;
    for p in &snapshot.processes {
        writeln!(
            file,
            "{},{},{},{},{},{},{},{},{},{},{}",
            csv_cell(&p.name),
            p.pid,
            p.parent_pid.map(|n| n.to_string()).unwrap_or_default(),
            if snapshot.cpu_ready {
                format!("{:.2}", p.cpu_usage)
            } else {
                String::new()
            },
            p.memory_bytes,
            p.gpu_usage.map(|n| format!("{n:.2}")).unwrap_or_default(),
            p.gpu_memory_bytes
                .map(|n| n.to_string())
                .unwrap_or_default(),
            p.disk_read_bytes_per_sec
                .map(|n| format!("{n:.2}"))
                .unwrap_or_default(),
            p.disk_write_bytes_per_sec
                .map(|n| format!("{n:.2}"))
                .unwrap_or_default(),
            p.start_time_unix_secs,
            csv_cell(
                &p.exe
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
            )
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::csv_cell;
    #[test]
    fn escapes_quotes_and_formula_prefixes() {
        assert_eq!(csv_cell("one,\"two\""), "\"one,\"\"two\"\"\"");
        assert_eq!(
            csv_cell(" =HYPERLINK(\"x\")"),
            "\"' =HYPERLINK(\"\"x\"\")\""
        );
        assert_eq!(csv_cell("app.exe"), "\"app.exe\"");
    }
}
