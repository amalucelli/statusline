// Claude Code's statusline: a JSON payload on stdin, one line on stdout.
//
// Claude Code reads only stdout and re-runs this on every turn, so the binary
// never fails visibly and never blocks — a component with nothing to say is
// left out rather than reported as an error.
//
// The split: `render` owns the line and its colour vocabulary, `usage` owns the
// rate-limit figures and the cache that keeps fetching them off the hot path,
// `time` owns durations and the reset countdown. What is left here is the
// payload's shape and the session clock, which is the one piece of state the
// payload cannot supply.

mod render;
mod time;
mod usage;

use anyhow::Result;
use clap::builder::styling::{AnsiColor, Styles};
use clap::{ArgAction, Parser};
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use std::time::SystemTime;

#[derive(Debug, Deserialize)]
struct StatuslineInput {
    model: Option<ModelInfo>,
    session_id: Option<String>,
    context_window: Option<ContextWindow>,
    cost: Option<CostInfo>,
    effort: Option<EffortInfo>,
}

#[derive(Debug, Deserialize)]
struct ModelInfo {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EffortInfo {
    level: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContextWindow {
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
    context_window_size: Option<u64>,
    used_percentage: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct CostInfo {
    total_duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SessionState {
    session_id: String,
    started_at_unix: u64,
}

const SESSION_STATE_PATH: &str = "/tmp/claude-code-status-session.json";
pub const REFRESH_FLAG: &str = "refresh-usage";

// `task install` sets STATUSLINE_DIRTY so a local build is distinguishable from
// a release carrying the same crate version.
const VERSION: &str = match option_env!("STATUSLINE_DIRTY") {
    Some(_) => concat!(env!("CARGO_PKG_VERSION"), "-dirty"),
    None => env!("CARGO_PKG_VERSION"),
};

const HELP_STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().bold())
    .usage(AnsiColor::Green.on_default().bold())
    .literal(AnsiColor::Cyan.on_default().bold())
    .placeholder(AnsiColor::Cyan.on_default());

#[derive(Parser)]
#[command(
    version = VERSION,
    disable_version_flag = true,
    about,
    styles = HELP_STYLES
)]
struct Cli {
    /// Print version
    #[arg(short, long, action = ArgAction::Version)]
    version: (),

    #[arg(long = REFRESH_FLAG, hide = true)]
    refresh_usage: bool,
}

fn main() {
    let cli = Cli::parse();
    if cli.refresh_usage {
        usage::refresh_usage_cache();
        return;
    }
    let _ = run();
}

fn run() -> Result<()> {
    let mut input_data = String::new();
    io::stdin().read_to_string(&mut input_data)?;

    let input: StatuslineInput = serde_json::from_str(&input_data)?;
    let plain = std::env::var("NO_COLOR").is_ok();

    let model_name = extract_model_name(&input);
    let context_percentage = input
        .context_window
        .as_ref()
        .and_then(|cw| cw.used_percentage)
        .unwrap_or(0.0);
    let context_window_size = input
        .context_window
        .as_ref()
        .and_then(|cw| cw.context_window_size)
        .unwrap_or(0);

    let session_duration_ms = match input.session_id.as_deref() {
        Some(sid) => wall_clock_session_duration_ms(sid),
        None => input
            .cost
            .as_ref()
            .and_then(|c| c.total_duration_ms)
            .unwrap_or(0),
    };
    let session_duration = time::format_duration_from_ms(session_duration_ms);
    let token_metrics = format_token_metrics_from_json(&input);
    let usage = usage::get_cached_usage();

    let output = render::format_statusline(
        &render::Model {
            name: &model_name,
            effort: input.effort.as_ref().and_then(|e| e.level.as_deref()),
        },
        context_percentage,
        context_window_size,
        &session_duration,
        &token_metrics,
        usage.as_ref(),
        plain,
    );
    println!("{}", output);
    Ok(())
}

fn extract_model_name(input: &StatuslineInput) -> String {
    input
        .model
        .as_ref()
        .and_then(|m| m.display_name.as_deref())
        .map(|name| name.trim_start_matches("Claude ").trim().to_owned())
        .unwrap_or_else(|| "Claude".to_owned())
}

fn wall_clock_session_duration_ms(session_id: &str) -> u64 {
    let now_unix = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let existing: Option<SessionState> = std::fs::read_to_string(SESSION_STATE_PATH)
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok());

    if let Some(state) = existing {
        if state.session_id == session_id {
            return now_unix.saturating_sub(state.started_at_unix) * 1000;
        }
    }

    let fresh = SessionState {
        session_id: session_id.to_owned(),
        started_at_unix: now_unix,
    };
    if let Ok(json) = serde_json::to_string(&fresh) {
        let tmp = format!("{}.tmp", SESSION_STATE_PATH);
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, SESSION_STATE_PATH);
        }
    }
    0
}

fn format_token_metrics_from_json(input: &StatuslineInput) -> String {
    let (input_tokens, output_tokens) = input
        .context_window
        .as_ref()
        .map(|cw| {
            (
                cw.total_input_tokens.unwrap_or(0),
                cw.total_output_tokens.unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));

    format!(
        "{}/{}",
        render::format_token_value(input_tokens),
        render::format_token_value(output_tokens)
    )
}
