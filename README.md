# shepherd

Watches GitHub for pull requests where you're a requested reviewer and automatically
opens a dedicated [herdr](https://herdr.dev) workspace/tab per PR, launches Claude Code
in it running a "principal engineer" review skill, and notifies you when the review is
ready. You get a row of tabs to triage instead of a mental todo list, and each one is a
live Claude Code session you can keep talking to.

v1 status: herdr only (no tmux backend yet), single-machine, foreground daemon.

## How it works

1. `shepherd daemon` polls `gh api search/issues` for open PRs where you're a requested
   reviewer, filtered to the repos you've allowlisted in config.
2. For each new-or-updated PR, it clones (or updates) the repo under a cache directory,
   checks out the PR's head into its own git worktree, and opens a tab for it in a
   shared `pr-review` herdr workspace (created on first use).
3. It launches Claude Code in that tab and submits `/principal-review <PR URL>` as the
   first message.
4. Once the turn finishes, it fires a system notification. The pane stays open and
   interactive - keep talking to it, or tell it to post the review to GitHub.

## Setup

Requires `gh`, `herdr`, `git`, and `claude` on your PATH, and `gh auth login` already
done.

```
cargo build --release
./target/release/shepherd init
```

`init` checks those dependencies, writes a default config to
`~/.config/shepherd/config.toml`, and installs the `principal-review` skill to
`~/.claude/skills/principal-review/SKILL.md`.

Add the repos you actually want watched:

```toml
[[repos]]
owner = "your-org"
name = "your-repo"
```

If you haven't already, run `herdr integration install claude` so herdr can track
Claude Code sessions' idle/working/done state - `shepherd init` will remind you if it
looks missing.

## Usage

```
shepherd review owner/repo 123   # review one PR right now, waits for it to finish
shepherd daemon                  # foreground poll loop over the allowlist
shepherd status                  # what's tracked in the dedup state file
```

## Posting to GitHub

The skill only drafts by default (summary, split-PR check, ranked findings, verdict,
draft comments) - it never posts on its own. Once you've read the draft in the pane,
just tell the agent what to do ("post this", "leave the first two comments", "approve
it") and it'll run the appropriate `gh pr review`/`gh pr comment` itself - it already
has `gh` access.

## Known limitations / things worth knowing

- **Trust**: shepherd marks each repo's base clone directory as trusted in
  `~/.claude.json` (`hasTrustDialogAccepted`) so Claude Code doesn't block on the
  first-run workspace-trust dialog. This only touches directories shepherd itself
  cloned from your allowlisted repos, and Claude Code resolves trust for a git
  worktree against its main repo's path, so it's a one-time thing per repo, not
  per PR.
- **Tool access**: the review session gets `Bash`, `Read`, `Grep`, `Glob` - no
  `Edit`/`Write`/`WebFetch`. `Bash` is intentionally unscoped rather than
  `Bash(git *) Bash(gh *)`: Claude Code's allowlist matching requires every part of a
  compound command (pipes, `;` chains, `$(...)`) to match, and ordinary review
  exploration chains git/gh through `head`/`grep`/`echo` constantly - a narrower list
  causes a permission prompt nobody's there to answer, which then hangs. The worktree
  it runs in is disposable and per-PR, not your real working directories.
- **If the skill ever needs a tool outside that set** (rare, but possible - e.g. it
  decides to try `WebFetch`), it'll hit a permission prompt with nobody there to answer
  it. `wait_until_finished` treats that as `blocked` and errors out rather than waiting
  out the full timeout, but the review itself won't complete - check the pane.
- **Dedup** is per-machine (a local JSON file), not shared. If you run the daemon on
  two machines at once, both could open a tab for the same PR.
- **Repo scope is allowlist-only** on purpose - an empty config means the daemon has
  nothing to watch, rather than firing on every repo you happen to have review access
  to.
- **Killing `shepherd` mid-review** (Ctrl-C, `kill`, machine sleep) can leave a tab
  behind whose Claude Code session launched but never got its first message - the
  process was interrupted before the "type the prompt in and submit" step. This is
  harmless: the PR is only marked reviewed after that step succeeds, so the next
  `daemon` poll (or a manual `shepherd review`) retries it cleanly. The stray empty
  tab itself isn't cleaned up automatically - close it by hand.
