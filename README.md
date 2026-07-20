# shep

Watches GitHub for pull requests where you're a requested reviewer and automatically
opens a tmux window per PR, launches Claude Code in it running a "principal engineer"
review skill, and notifies you when the review is ready. You get a row of windows to
triage instead of a mental todo list, and each one is a live Claude Code session you
can keep talking to.

v1 status: single-machine, foreground daemon.

## How it works

1. `shep daemon` polls `gh api search/issues` for open PRs where you're a requested
   reviewer, filtered to the repos you've allowlisted in config.
2. For each new-or-updated PR, it clones (or updates) the repo under a cache directory,
   checks out the PR's head into its own git worktree, and opens a window for it in a
   shared `shep` tmux session (created on first use).
3. It launches Claude Code in that window and submits `/principal-review <PR URL>` as
   the first message.
4. Once the turn finishes, it fires a system notification. The session stays open and
   interactive - keep talking to it, or tell it to post the review to GitHub.
5. Once a PR is merged/closed on GitHub and its review has been sitting done for a
   few days, shep closes the window and removes the local worktree automatically.

## Setup

Requires `gh`, `tmux`, `git`, and `claude` on your PATH, and `gh auth login` already
done.

```
cargo install --path .
shep init
```

`cargo install --path .` builds and puts `shep` on your PATH via `~/.cargo/bin`. `init`
checks the dependencies above, writes a default config to `~/.config/shep/config.toml`,
and installs the `principal-review` skill to `~/.claude/skills/principal-review/SKILL.md`.

A few config knobs worth knowing about, all with sane defaults:
- `lookback_days` (default `1`) - only considers PRs you were tagged as a reviewer on
  within this window, not every PR you've ever been asked to review.
- `max_triggers_per_poll` (default `3`) - caps how many reviews fire in one poll, so a
  backlog (e.g. after being away) gets spread across multiple polls instead of firing
  all at once.
- `cleanup_after_days` (default `3`) - grace period before a merged/closed PR's window
  and worktree get cleaned up automatically. `0` disables cleanup entirely.

Add the repos you actually want watched:

```toml
[[repos]]
owner = "your-org"
name = "your-repo"
```

## Usage

```
shep review owner/repo 123   # review one PR right now, waits for it to finish
shep daemon                  # foreground poll loop over the allowlist
shep status                  # what's tracked in the dedup state file
```

Reviews happen in a tmux session named `shep` (configurable via `tmux_session` in
config). `tmux attach -t shep` to look in on it from anywhere, including from inside
another multiplexer like herdr - tmux doesn't care what's hosting its terminal.
`Ctrl-b d` to detach without killing it.

## Posting to GitHub

The skill only drafts by default (summary, split-PR check, ranked findings, verdict,
draft comments) - it never posts on its own. Once you've read the draft, just tell the
agent what to do ("post this", "leave the first two comments", "approve it") and it'll
run the appropriate `gh pr review`/`gh pr comment` itself - it already has `gh` access.

## Known limitations / things worth knowing

- **Trust**: shep marks each repo's base clone directory as trusted in
  `~/.claude.json` (`hasTrustDialogAccepted`) so Claude Code doesn't block on the
  first-run workspace-trust dialog. This only touches directories shep itself cloned
  from your allowlisted repos, and Claude Code resolves trust for a git worktree
  against its main repo's path, so it's a one-time thing per repo, not per PR.
- **Tool access**: the review session gets `Bash`, `Read`, `Grep`, `Glob` - no
  `Edit`/`Write`/`WebFetch`. `Bash` is intentionally unscoped rather than
  `Bash(git *) Bash(gh *)`: Claude Code's allowlist matching requires every part of a
  compound command (pipes, `;` chains, `$(...)`) to match, and ordinary review
  exploration chains git/gh through `head`/`grep`/`echo` constantly - a narrower list
  causes a permission prompt nobody's there to answer, which then hangs. The worktree
  it runs in is disposable and per-PR, not your real working directories.
- **Completion detection** is a Claude Code `Stop` hook (configured per-launch via
  `--settings`) that touches a sentinel file when a turn finishes; shep waits on that
  file. If the skill ever needs a tool outside the allowed set, it'll hit a permission
  prompt nobody's there to answer, the turn never finishes, the hook never fires, and
  `shep` just times out at 900s waiting - check the window.
- **Notifications are macOS-only** for now (`osascript`). A `notify-send` branch for
  Linux would be a small addition if needed.
- **Dedup** is per-machine (a local JSON file), not shared - if you run the daemon on
  two machines at once, both could open a window for the same PR. On a single machine,
  a second `shep daemon` refuses to start while one's already running (a PID lock), so
  they can't race each other locally.
- **Repo scope is allowlist-only** on purpose - an empty config means the daemon has
  nothing to watch, rather than firing on every repo you happen to have review access
  to.
- **Each PR's state is `reviewing` or `reviewed`**, visible via `shep status`. A
  `reviewing` entry only counts as genuinely in progress if its tmux window is still
  alive - if you kill it yourself (or `shep` gets killed mid-review before the prompt
  is even submitted), the next poll notices the window's gone and retries instead of
  treating it as silently done forever. The one gap: this retry check only runs for
  PRs that still pass the `lookback_days` filter, so a `reviewing` entry that goes
  quiet and ages out of that window won't be reconsidered automatically.
