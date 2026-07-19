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
    /// Set when this call created the workspace: `workspace create` makes an
    /// unused starter tab alongside it. herdr won't close a workspace's last
    /// tab, so the caller must close this only after adding a real one.
    pub starter_tab_id: Option<String>,
}

/// Finds the workspace with the given label, creating it if needed.
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

/// Returns Err rather than swallowing failures itself; callers that want
/// best-effort should log the error rather than discard it.
pub fn close_tab(tab_id: &str) -> Result<()> {
    run_herdr_ok(&["tab", "close", tab_id])
}

/// Creates a tab cwd'd into the PR worktree; returns its id and root pane id.
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

/// Types `command` into the pane's shell + Enter. Fire-and-forget: returns
/// once herdr accepts it, not once it finishes.
pub fn run_in_pane(pane_id: &str, command: &str) -> Result<()> {
    run_herdr_ok(&["pane", "run", pane_id, command])
}

/// Writes text into the pane without submitting it. `claude`'s CLI prompt
/// argument isn't reliably picked up as the first turn in interactive mode,
/// so the prompt is typed in and submitted separately instead.
pub fn send_text(pane_id: &str, text: &str) -> Result<()> {
    run_herdr_ok(&["pane", "send-text", pane_id, text])
}

pub fn send_enter(pane_id: &str) -> Result<()> {
    run_herdr_ok(&["pane", "send-keys", pane_id, "enter"])
}

/// Blocks until `text` appears in the pane's output. Used over
/// `agent_status: idle` for launch-readiness: that status fires for the
/// plain shell the instant the `claude ...` command is submitted, before
/// Claude Code itself has started - input sent then gets silently dropped.
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

/// Polls until the turn in `pane_id` finishes (up to `timeout`), rather than
/// using `herdr wait agent-status`: that only fires on a transition *into* a
/// given status, so it can miss one that already happened and hang, and
/// interactive completion lands on "done", not "idle". Polling treats either
/// as finished once "working" has been seen, and errors immediately on
/// "blocked" (e.g. an unanswered permission prompt) instead of timing out.
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
