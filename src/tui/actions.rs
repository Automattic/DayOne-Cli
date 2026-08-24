use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};

/// Copy `text` to the OS clipboard using the platform's native CLI helper.
pub fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut cmd = clipboard_command()?;
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().context("failed to launch clipboard helper")?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("clipboard helper did not expose stdin"))?;
        stdin
            .write_all(text.as_bytes())
            .context("failed to write text to clipboard helper")?;
    }
    let status = child.wait().context("clipboard helper did not exit")?;
    if !status.success() {
        return Err(anyhow!("clipboard helper exited with status {status}",));
    }
    Ok(())
}

fn clipboard_command() -> Result<Command> {
    if cfg!(target_os = "macos") {
        return Ok(Command::new("pbcopy"));
    }
    if cfg!(target_os = "linux") {
        if let Ok(()) = which("wl-copy") {
            return Ok(Command::new("wl-copy"));
        }
        if let Ok(()) = which("xclip") {
            let mut cmd = Command::new("xclip");
            cmd.args(["-selection", "clipboard"]);
            return Ok(cmd);
        }
        if let Ok(()) = which("xsel") {
            let mut cmd = Command::new("xsel");
            cmd.args(["--clipboard", "--input"]);
            return Ok(cmd);
        }
        return Err(anyhow!(
            "no clipboard helper found (install wl-copy, xclip, or xsel)"
        ));
    }
    if cfg!(target_os = "windows") {
        return Ok(Command::new("clip"));
    }
    Err(anyhow!("no known clipboard helper on this platform"))
}

fn which(bin: &str) -> Result<()> {
    let path = std::env::var_os("PATH").ok_or_else(|| anyhow!("PATH not set"))?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Ok(());
        }
    }
    Err(anyhow!("not found"))
}
