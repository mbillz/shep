use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub struct Pane {
    pub tab_id: String,
    pub pane_id: String,
}

fn run_herdr_json(args: &[&str]) -> Result<Value> {
    let output = Command::new("herdr")
        .args(args)
        .output()
        .with_context(|| format!("failed to run `herdr {}`", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "`herdr {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: Value = serde_json::from_str(&stdout)
        .with_context(|| format!("parsing `herdr {}` output as JSON: {stdout}", args.join(" ")))?;
    envelope
        .get("result")
        .cloned()
        .with_context(|| format!("`herdr {}` response had no `result` field", args.join(" ")))
}

fn run_herdr_ok(args: &[&str]) -> Result<()> {
    let output = Command::new("herdr")
        .args(args)
        .output()
        .with_context(|| format!("failed to run `herdr {}`", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "`herdr {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct WorkspaceInfo {
    workspace_id: String,
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Snapshot {
    workspaces: Vec<WorkspaceInfo>,
}

pub struct Workspace {
    pub workspace_id: String,
    /// Set when this call created the workspace: `workspace create` always
    /// makes a starter tab alongside it, which we don't use (every review
    /// gets its own tab via create_tab). herdr refuses to close a
    /// workspace's *last* tab, so this can't be closed here - the caller
    /// must close it after adding a real tab, once it's no longer the last.
    pub starter_tab_id: Option<String>,
}

/// Finds the workspace with the given label, creating it if it doesn't
/// exist yet. This is the shared "review space" all PR tabs get created in.
pub fn ensure_workspace(label: &str) -> Result<Workspace> {
    let result = run_herdr_json(&["api", "snapshot"])?;
    let snapshot: Snapshot = serde_json::from_value(
        result
            .get("snapshot")
            .cloned()
            .context("herdr api snapshot response had no `snapshot` field")?,
    )?;
    if let Some(ws) = snapshot
        .workspaces
        .into_iter()
        .find(|w| w.label.as_deref() == Some(label))
    {
        return Ok(Workspace {
            workspace_id: ws.workspace_id,
            starter_tab_id: None,
        });
    }

    let result = run_herdr_json(&["workspace", "create", "--label", label, "--no-focus"])?;
    let workspace_id: String = result
        .get("workspace")
        .and_then(|w| w.get("workspace_id"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .context("herdr workspace create response had no workspace.workspace_id")?;
    let starter_tab_id = result
        .get("tab")
        .and_then(|t| t.get("tab_id"))
        .and_then(|v| v.as_str())
        .map(String::from);

    Ok(Workspace {
        workspace_id,
        starter_tab_id,
    })
}

/// Closes a tab. Not wrapped in a broader "best-effort, ignore failures"
/// helper: a silently-swallowed failure here is exactly how the starter-tab
/// cleanup bug (closing a workspace's only tab, which herdr rejects) went
/// unnoticed - callers that want it best-effort should log the error
/// themselves, not discard it.
pub fn close_tab(tab_id: &str) -> Result<()> {
    run_herdr_ok(&["tab", "close", tab_id])
}

/// Creates a new tab in the given workspace, cwd'd into the PR worktree, and
/// returns the tab's id and its root pane's id.
pub fn create_tab(workspace_id: &str, cwd: &Path, label: &str) -> Result<Pane> {
    let cwd_str = cwd.to_str().context("worktree path is not valid UTF-8")?;
    let result = run_herdr_json(&[
        "tab",
        "create",
        "--workspace",
        workspace_id,
        "--cwd",
        cwd_str,
        "--label",
        label,
        "--no-focus",
    ])?;
    let tab_id = result
        .get("tab")
        .and_then(|t| t.get("tab_id"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .context("herdr tab create response had no tab.tab_id")?;
    let pane_id = result
        .get("root_pane")
        .and_then(|p| p.get("pane_id"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .context("herdr tab create response had no root_pane.pane_id")?;
    Ok(Pane { tab_id, pane_id })
}

/// Types `command` into the pane's shell followed by Enter. Fire-and-forget:
/// this returns as soon as herdr accepts the command, not when it finishes.
pub fn run_in_pane(pane_id: &str, command: &str) -> Result<()> {
    run_herdr_ok(&["pane", "run", pane_id, command])
}

/// Writes literal text into the pane without submitting it. Used to seed a
/// Claude Code session's first message: passing the prompt as a CLI argument
/// to `claude` is not reliably picked up as the initial turn in interactive
/// mode (verified empirically - it launches but never processes it), but
/// typing it in and submitting separately works the same as a human doing it.
pub fn send_text(pane_id: &str, text: &str) -> Result<()> {
    run_herdr_ok(&["pane", "send-text", pane_id, text])
}

pub fn send_enter(pane_id: &str) -> Result<()> {
    run_herdr_ok(&["pane", "send-keys", pane_id, "enter"])
}

/// Blocks (up to `timeout`) until `text` appears in the pane's rendered
/// output. Used instead of `wait_agent_status(..., "idle", ...)` to detect
/// that Claude Code's TUI has actually finished rendering after launch:
/// agent_status reports "idle" for the plain shell the moment `pane run`'s
/// `claude ...` command line is submitted, before Claude Code itself has
/// started - sending input at that point lands on a not-yet-ready terminal
/// and gets silently dropped (verified empirically). Matching on UI text
/// that only exists once Claude Code has rendered avoids that race.
pub fn wait_for_text(pane_id: &str, text: &str, timeout: Duration) -> Result<()> {
    run_herdr_ok(&[
        "wait",
        "output",
        pane_id,
        "--match",
        text,
        "--timeout",
        &timeout.as_millis().to_string(),
    ])
}

#[derive(Debug, Deserialize)]
struct PaneAgentStatus {
    agent_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PaneGetResult {
    pane: PaneAgentStatus,
}

fn agent_status(pane_id: &str) -> Result<String> {
    let result = run_herdr_json(&["pane", "get", pane_id])?;
    let parsed: PaneGetResult = serde_json::from_value(result)?;
    Ok(parsed.pane.agent_status.unwrap_or_else(|| "unknown".to_string()))
}

/// Polls (rather than using `herdr wait agent-status`) until the turn in
/// `pane_id` finishes, up to `timeout`.
///
/// Claude Code reports idle/working/blocked/done as genuinely distinct
/// values, not just "busy or not" - and interactive-mode completion lands on
/// "done", not "idle" (verified empirically: a completed review sat at
/// `agent_status: "done"` while a wait on "idle" hung indefinitely).
/// `wait agent-status --status X` also only fires on a transition *into* X;
/// if that transition already happened before the wait call started
/// listening, it hangs until X recurs, which may be never. Polling avoids
/// both problems: it only needs "working" to have been seen at some point,
/// then treats either "idle" or "done" as finished, and surfaces "blocked"
/// (e.g. an unanswered permission prompt) as an immediate error rather than
/// silently waiting out the full timeout.
pub fn wait_until_finished(pane_id: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut seen_working = false;
    loop {
        match agent_status(pane_id)?.as_str() {
            "working" => seen_working = true,
            "idle" | "done" if seen_working => return Ok(()),
            "blocked" => bail!(
                "agent in pane {pane_id} is blocked waiting on input (likely an unanswered permission prompt) - check the pane"
            ),
            _ => {}
        }
        if Instant::now() >= deadline {
            bail!("timed out after {:?} waiting for the review to finish", timeout);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

pub fn notify(title: &str, body: &str) -> Result<()> {
    run_herdr_ok(&[
        "notification",
        "show",
        title,
        "--body",
        body,
        "--sound",
        "done",
    ])
}
