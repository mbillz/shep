mod claude_trust;
mod config;
mod daemon;
mod git;
mod github;
mod notify;
mod paths;
mod review;
mod skill;
mod state;
mod tmux;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use github::PrRef;
use state::State;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "shep", about = "\u{1f415} Auto-launches principal-engineer PR reviews in tmux")]
struct Cli {
    /// Defaults to `daemon` if omitted.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Check dependencies, write default config, install the review skill.
    Init,
    /// Review a single PR right now, e.g. `shep review acme/widgets 42`.
    Review {
        /// owner/repo, e.g. acme/widgets
        repo: String,
        number: u64,
    },
    /// Poll the configured repos in the foreground and review new/updated PRs.
    Daemon,
    /// Show what's currently tracked in the dedup state file.
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Daemon) {
        Command::Init => cmd_init(),
        Command::Review { repo, number } => cmd_review(&repo, number),
        Command::Daemon => cmd_daemon(),
        Command::Status => cmd_status(),
    }
}

fn check_dependency(bin: &str) -> bool {
    // tmux doesn't support GNU-style --version, only -V.
    let version_flag = if bin == "tmux" { "-V" } else { "--version" };
    std::process::Command::new(bin)
        .arg(version_flag)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn cmd_init() -> Result<()> {
    let mut missing = Vec::new();
    for bin in ["gh", "tmux", "claude", "git"] {
        if check_dependency(bin) {
            println!("found {bin}");
        } else {
            missing.push(bin);
        }
    }
    if !missing.is_empty() {
        bail!("missing required tools on PATH: {}", missing.join(", "));
    }

    let config = Config::default();
    if config.write_if_missing()? {
        println!("wrote default config to {}", Config::path()?.display());
        println!("  edit it to add repos under [[repos]] before running `shep daemon`");
    } else {
        println!("config already exists at {}", Config::path()?.display());
    }

    skill::install()?;
    println!(
        "installed principal-review skill to {}",
        paths::claude_skills_dir()?.join("principal-review/SKILL.md").display()
    );
    println!("\u{1f415} ready to fetch");

    Ok(())
}

fn parse_repo(repo: &str) -> Result<(String, String)> {
    let (owner, name) = repo
        .split_once('/')
        .with_context(|| format!("expected owner/repo, got {repo}"))?;
    Ok((owner.to_string(), name.to_string()))
}

fn cmd_review(repo: &str, number: u64) -> Result<()> {
    let (owner, name) = parse_repo(repo)?;
    let config = Config::load_or_default()?;
    let pr = PrRef {
        owner: owner.clone(),
        repo: name.clone(),
        number,
    };

    println!("\u{1f415} triggering review for {}", pr.full_ref());
    let details = github::pr_view(&pr)?;
    let triggered = review::trigger_review(&config, &pr, details)?;
    println!(
        "opened window {} in the '{}' tmux session",
        triggered.window_id, config.tmux_session
    );

    let mut state = State::load_or_default()?;
    state.mark_reviewed(&owner, &name, number, &triggered.details.head_sha);
    state.save()?;

    println!("waiting for the review to finish...");
    review::await_and_notify(&pr, &triggered, Duration::from_secs(900))?;
    println!(
        "\u{1f415} review ready - `tmux attach -t {}` to see it",
        config.tmux_session
    );

    Ok(())
}

fn cmd_daemon() -> Result<()> {
    let config = Config::load_or_default()?;
    daemon::run(&config)
}

fn cmd_status() -> Result<()> {
    let config = Config::load_or_default()?;
    println!("config: {}", Config::path()?.display());
    println!("watching {} repo(s):", config.repos.len());
    for r in &config.repos {
        println!("  {}", r.full_name());
    }

    let state = State::load_or_default()?;
    println!("\ntracked PRs:");
    let mut entries: Vec<_> = state.entries().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    if entries.is_empty() {
        println!("  (none yet)");
    }
    for (key, entry) in entries {
        println!("  {key}  sha={}  reviewed_at={}", &entry.last_sha[..entry.last_sha.len().min(8)], entry.reviewed_at);
    }

    Ok(())
}
