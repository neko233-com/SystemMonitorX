//! Explicit diagnostic: check the public release API and optionally download
//! its MSI to a user-specified directory. Never starts an installer.
use anyhow::{Result, bail};
use system_monitor_x::updater::{CheckResponse, check_latest, download_verified};
fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let directory = match arguments.next().as_deref() {
        None | Some("--check") => None,
        Some("--download-to") => {
            Some(std::path::PathBuf::from(arguments.next().ok_or_else(
                || anyhow::anyhow!("--download-to needs a directory"),
            )?))
        }
        _ => bail!("Usage: update_probe [--check | --download-to DIRECTORY]"),
    };
    if let CheckResponse::New(release, _) = check_latest(None)? {
        if let Some(directory) = directory {
            let path = download_verified(&release, &directory, |_, _| {})?;
            println!("Verified MSI: {}", path.display());
        }
        println!("{}", serde_json::to_string_pretty(&release)?);
    }
    Ok(())
}
