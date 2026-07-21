use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn run_tmux(args: &[&str]) -> Result<String> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .with_context(|| format!("failed to run `tmux {}`", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "`tmux {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn session_exists(session: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Gets a window for a review: creates the session (naming this as its first
/// window) if it doesn't exist yet, otherwise adds a window to it. Returns
/// the window's stable `#{window_id}` (e.g. `@3`) for later targeting.
pub fn create_window(session: &str, cwd: &Path, label: &str) -> Result<String> {
    let cwd_str = cwd.to_str().context("worktree path is not valid UTF-8")?;
    let args: Vec<&str> = if session_exists(session) {
        vec![
            "new-window", "-t", session, "-n", label, "-c", cwd_str, "-P", "-F", "#{window_id}",
        ]
    } else {
        vec![
            "new-session", "-d", "-s", session, "-n", label, "-c", cwd_str, "-P", "-F",
            "#{window_id}",
        ]
    };
    run_tmux(&args)
}

/// Makes sure the session exists (with an idle placeholder window) even
/// before any review has been triggered, so `tmux attach -t <session>`
/// works right away. No-op if it already exists.
pub fn ensure_session(session: &str, cwd: &Path) -> Result<()> {
    if session_exists(session) {
        return Ok(());
    }
    create_window(session, cwd, "idle").map(|_| ())
}

/// True if the session has a window with this name.
pub fn window_named_exists(session: &str, name: &str) -> bool {
    Command::new("tmux")
        .args(["list-windows", "-t", session, "-F", "#{window_name}"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).lines().any(|l| l == name))
        .unwrap_or(false)
}

/// Single-quotes `s` for use as one word in a shell command line.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Creates a window that runs `command` instead of sitting at an idle shell
/// - used for the daemon's own window so `shep watch` can show it polling
/// in tab 0. `remain-on-exit` keeps the window (and its output) around if
/// the command exits instead of the window just vanishing.
pub fn create_command_window(session: &str, cwd: &Path, label: &str, command: &str) -> Result<String> {
    let cwd_str = cwd.to_str().context("path is not valid UTF-8")?;
    let quoted = shell_quote(command);
    let args: Vec<&str> = if session_exists(session) {
        vec![
            "new-window", "-t", session, "-n", label, "-c", cwd_str, "-P", "-F", "#{window_id}", &quoted,
        ]
    } else {
        vec![
            "new-session", "-d", "-s", session, "-n", label, "-c", cwd_str, "-P", "-F", "#{window_id}", &quoted,
        ]
    };
    let window_id = run_tmux(&args)?;
    run_tmux(&["set-window-option", "-t", &window_id, "remain-on-exit", "on"])?;
    Ok(window_id)
}

/// Replaces the current process with `tmux attach -t session`, landing the
/// caller directly inside the session instead of leaving it as a bystander
/// foreground process printing to its own terminal.
pub fn attach(session: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let err = Command::new("tmux").args(["attach-session", "-t", session]).exec();
    bail!("failed to exec `tmux attach-session -t {session}`: {err}")
}

pub fn kill_window(window_id: &str) -> Result<()> {
    run_tmux(&["kill-window", "-t", window_id]).map(|_| ())
}

/// Whether `window_id` still exists - used to tell a genuinely in-progress
/// review apart from one whose window got closed (e.g. killed by hand).
pub fn window_exists(window_id: &str) -> bool {
    Command::new("tmux")
        .args(["list-panes", "-t", window_id])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Writes literal text into the window without submitting it.
pub fn send_text(window_id: &str, text: &str) -> Result<()> {
    run_tmux(&["send-keys", "-t", window_id, text]).map(|_| ())
}

pub fn send_enter(window_id: &str) -> Result<()> {
    run_tmux(&["send-keys", "-t", window_id, "Enter"]).map(|_| ())
}

/// Polls the window's rendered content until `text` appears, up to `timeout`.
pub fn wait_for_text(window_id: &str, text: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let content = run_tmux(&["capture-pane", "-t", window_id, "-p"])?;
        if content.contains(text) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out after {:?} waiting for {text:?} to appear in {window_id}", timeout);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
