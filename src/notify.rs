use anyhow::{bail, Context, Result};
use std::process::Command;

fn applescript_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Fires a macOS notification via osascript. macOS only for now.
pub fn notify(title: &str, body: &str) -> Result<()> {
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_quote(body),
        applescript_quote(title),
    );
    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .context("failed to run osascript")?;
    if !output.status.success() {
        bail!(
            "osascript failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
