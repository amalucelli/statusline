use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

#[derive(Debug, Deserialize)]
struct StatuslineInput {
    model: Option<ModelInfo>,
    session_id: Option<String>,
    context_window: Option<ContextWindow>,
    cost: Option<CostInfo>,
}

#[derive(Debug, Deserialize)]
struct ModelInfo {
    display_name: Option<String>,
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
struct UsageResponse {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
}

#[derive(Debug, Deserialize, Serialize)]
struct UsageWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SessionState {
    session_id: String,
    started_at_unix: u64,
}

const SEPARATOR_COLOR: &str = "\x1b[38;5;175m";
const INPUT_TOKEN_COLOR: &str = "\x1b[38;5;150m";
const OUTPUT_TOKEN_COLOR: &str = "\x1b[38;5;180m";
const ICON_COLOR: &str = "\x1b[38;5;245m";
const RESET: &str = "\x1b[0m";

const GRADIENT_COLORS: [&str; 6] = [
    "\x1b[38;5;230m",
    "\x1b[38;5;223m",
    "\x1b[38;5;216m",
    "\x1b[38;5;209m",
    "\x1b[38;5;202m",
    "\x1b[38;5;166m",
];

const KILOBYTE_THRESHOLD: u64 = 1_000;
const MEGABYTE_THRESHOLD: u64 = 1_000_000;

const CACHE_PATH: &str = "/tmp/claude-code-status-usage.json";
const REFRESH_LOCK_PATH: &str = "/tmp/claude-code-status-usage.refresh.lock";
const SESSION_STATE_PATH: &str = "/tmp/claude-code-status-session.json";
const CACHE_TTL_EMPTY_SECS: u64 = 60;
const CACHE_TTL_FULL_SECS: u64 = 120;
const REFRESH_LOCK_STALE_SECS: u64 = 60;
const REFRESH_ARG: &str = "--refresh-usage";
const AUTOCOMPACT_WARN_PCT: f64 = 70.0;

fn main() {
    if std::env::args().any(|a| a == REFRESH_ARG) {
        refresh_usage_cache();
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
    let session_duration = format_duration_from_ms(session_duration_ms);
    let token_metrics = format_token_metrics_from_json(&input);
    let usage = get_cached_usage();

    let output = format_statusline(
        &model_name,
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

fn format_duration_from_ms(ms: u64) -> String {
    let total_minutes = ms / 60_000;
    match total_minutes {
        0 => "0m".to_owned(),
        m if m < 60 => format!("{}m", m),
        m => {
            let hours = m / 60;
            let minutes = m % 60;
            format!("{}h{}m", hours, minutes)
        }
    }
}

fn parse_rfc3339_to_unix(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    if bytes.len() < 20 {
        return None;
    }

    let parse_u32 = |start: usize, len: usize| -> Option<u32> {
        std::str::from_utf8(&bytes[start..start + len])
            .ok()?
            .parse()
            .ok()
    };

    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }

    let year = parse_u32(0, 4)?;
    let month = parse_u32(5, 2)?;
    let day = parse_u32(8, 2)?;
    let hour = parse_u32(11, 2)?;
    let minute = parse_u32(14, 2)?;
    let second = parse_u32(17, 2)?;

    let mut idx = 19;
    if idx < bytes.len() && bytes[idx] == b'.' {
        idx += 1;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
    }

    let (offset_secs, sign) = match bytes.get(idx) {
        Some(&b'Z') => (0i64, 1i64),
        Some(&b'+') | Some(&b'-') => {
            let sign = if bytes[idx] == b'-' { -1i64 } else { 1i64 };
            if idx + 6 > bytes.len() || bytes[idx + 3] != b':' {
                return None;
            }
            let oh = parse_u32(idx + 1, 2)? as i64;
            let om = parse_u32(idx + 4, 2)? as i64;
            (oh * 3600 + om * 60, sign)
        }
        _ => return None,
    };

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    let y = year as i64;
    let m = month as i64;
    let d = day as i64;

    let (y_adj, m_adj) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    let era = y_adj.div_euclid(400);
    let yoe = y_adj.rem_euclid(400);
    let doy = (153 * (m_adj - 3) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = era * 146097 + doe - 719468;

    let total =
        days_since_epoch * 86400 + (hour as i64) * 3600 + (minute as i64) * 60 + second as i64
            - sign * offset_secs;

    if total < 0 {
        None
    } else {
        Some(total as u64)
    }
}

fn format_reset_countdown(resets_at: &str) -> Option<String> {
    let reset_unix = parse_rfc3339_to_unix(resets_at)?;
    let now_unix = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();

    if reset_unix <= now_unix {
        return None;
    }

    let remaining = reset_unix - now_unix;
    let total_minutes = remaining / 60;

    if total_minutes == 0 {
        return Some("<1m".to_owned());
    }

    let days = total_minutes / 1440;
    let hours = (total_minutes % 1440) / 60;
    let minutes = total_minutes % 60;

    Some(if days > 0 {
        format!("{}d{}h", days, hours)
    } else if hours > 0 {
        format!("{}h{}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    })
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
        format_token_value(input_tokens),
        format_token_value(output_tokens)
    )
}

fn format_token_value(tokens: u64) -> String {
    match tokens {
        t if t >= MEGABYTE_THRESHOLD => {
            let millions = t as f64 / MEGABYTE_THRESHOLD as f64;
            if millions >= 10.0 {
                format!("{:.0}M", millions)
            } else {
                format!("{:.1}M", millions)
            }
        }
        t if t >= KILOBYTE_THRESHOLD => format!("{}k", t / KILOBYTE_THRESHOLD),
        t => format!("{}", t),
    }
}

fn get_session_duration_color(total_minutes: u64, plain: bool) -> &'static str {
    if plain {
        return "";
    }
    match total_minutes {
        m if m >= 600 => "\x1b[38;5;94m",
        m if m >= 480 => "\x1b[38;5;137m",
        m if m >= 360 => "\x1b[38;5;138m",
        m if m >= 240 => "\x1b[38;5;180m",
        m if m >= 120 => "\x1b[38;5;186m",
        m if m >= 60 => "\x1b[38;5;187m",
        m if m >= 30 => "\x1b[38;5;222m",
        m if m >= 10 => "\x1b[38;5;223m",
        _ => "\x1b[38;5;245m",
    }
}

// The 1M-context models auto-compact at the 500k "auto window" — half the full
// window — but the statusline JSON reports used_percentage against the full
// window, so it reads ~47% just as compaction fires. Standard windows have no
// such split. Half is the default; /autocompact can change it and the real
// threshold isn't exposed here.
fn autocompact_budget(context_window_size: u64) -> Option<u64> {
    (context_window_size > 200_000).then_some(context_window_size / 2)
}

fn get_context_color(percentage: f64, plain: bool) -> &'static str {
    if plain {
        return "";
    }
    match percentage {
        p if p >= 95.0 => "\x1b[38;5;203m",
        p if p >= 90.0 => "\x1b[38;5;180m",
        p if p >= 80.0 => "\x1b[38;5;222m",
        _ => "\x1b[38;5;223m",
    }
}

fn get_usage_color(utilization: f64, plain: bool) -> &'static str {
    if plain {
        return "";
    }
    match utilization {
        u if u >= 90.0 => "\x1b[38;5;203m",
        u if u >= 75.0 => "\x1b[38;5;180m",
        u if u >= 50.0 => "\x1b[38;5;222m",
        _ => "\x1b[38;5;223m",
    }
}

fn format_gradient_text(text: &str, plain: bool) -> String {
    if plain {
        return text.to_owned();
    }

    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let segment_size = len / GRADIENT_COLORS.len();

    let mut result = String::with_capacity(len * 15);
    for (i, ch) in chars.iter().enumerate() {
        let color_idx = if segment_size == 0 {
            0
        } else {
            (i / segment_size).min(GRADIENT_COLORS.len() - 1)
        };
        result.push_str(GRADIENT_COLORS[color_idx]);
        result.push(*ch);
    }
    result
}

fn parse_duration_to_minutes(duration_str: &str) -> u64 {
    if duration_str == "0m" {
        return 0;
    }

    match duration_str.find('h') {
        Some(h_pos) => {
            let hours = duration_str[..h_pos].parse::<u64>().unwrap_or(0) * 60;
            let minutes = duration_str
                .rfind('m')
                .and_then(|m_pos| duration_str[h_pos + 1..m_pos].parse::<u64>().ok())
                .unwrap_or(0);
            hours + minutes
        }
        None => duration_str
            .find('m')
            .and_then(|m_pos| duration_str[..m_pos].parse::<u64>().ok())
            .unwrap_or(0),
    }
}

fn get_oauth_token() -> Option<String> {
    // macOS keeps the Claude Code OAuth token in the login keychain;
    // Linux has no keychain and Claude Code writes it to a file instead.
    // Both hold the same {"claudeAiOauth":{"accessToken":...}} JSON.
    let raw = keychain_credentials().or_else(file_credentials)?;
    let parsed: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    parsed
        .get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_owned())
}

fn keychain_credentials() -> Option<String> {
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    String::from_utf8(output.stdout).ok()
}

fn file_credentials() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    std::fs::read_to_string(std::path::Path::new(&home).join(".claude/.credentials.json")).ok()
}

struct IPv4FirstResolver;

impl ureq::Resolver for IPv4FirstResolver {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<std::net::SocketAddr>> {
        let addrs: Vec<std::net::SocketAddr> =
            std::net::ToSocketAddrs::to_socket_addrs(&netloc)?.collect();
        let (v4, v6): (Vec<_>, Vec<_>) = addrs.into_iter().partition(|a| a.is_ipv4());
        Ok(v4.into_iter().chain(v6).collect())
    }
}

fn fetch_usage(token: &str) -> Option<UsageResponse> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(Duration::from_secs(3))
        .timeout_write(Duration::from_secs(3))
        .resolver(IPv4FirstResolver)
        .build();

    let response = agent
        .get("https://api.anthropic.com/api/oauth/usage")
        .set("Authorization", &format!("Bearer {}", token))
        .set("anthropic-beta", "oauth-2025-04-20")
        .call()
        .ok()?;

    serde_json::from_reader(response.into_reader()).ok()
}

fn get_cached_usage() -> Option<UsageResponse> {
    let cache_path = std::path::Path::new(CACHE_PATH);

    let cache_age = cache_path
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age.as_secs());

    let parsed = std::fs::read_to_string(cache_path)
        .ok()
        .and_then(|data| serde_json::from_str::<UsageResponse>(&data).ok());

    let has_data = parsed.as_ref().is_some_and(|u| {
        u.five_hour.as_ref().and_then(|w| w.utilization).is_some()
            || u.seven_day.as_ref().and_then(|w| w.utilization).is_some()
    });
    let ttl = if has_data {
        CACHE_TTL_FULL_SECS
    } else {
        CACHE_TTL_EMPTY_SECS
    };

    let needs_refresh = match cache_age {
        Some(age) => age >= ttl || parsed.is_none(),
        None => true,
    };

    if needs_refresh {
        spawn_background_refresh();
    }

    parsed
}

fn spawn_background_refresh() {
    let lock_path = std::path::Path::new(REFRESH_LOCK_PATH);

    let lock_is_stale = lock_path
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age.as_secs() >= REFRESH_LOCK_STALE_SECS)
        .unwrap_or(false);

    if lock_is_stale {
        let _ = std::fs::remove_file(lock_path);
    }

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)
    {
        Ok(_) => {}
        Err(_) => return,
    }

    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => {
            let _ = std::fs::remove_file(lock_path);
            return;
        }
    };

    let spawn_result = std::process::Command::new(exe)
        .arg(REFRESH_ARG)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    if spawn_result.is_err() {
        let _ = std::fs::remove_file(lock_path);
    }
}

fn refresh_usage_cache() {
    struct LockGuard;
    impl Drop for LockGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(REFRESH_LOCK_PATH);
        }
    }
    let _guard = LockGuard;

    let Some(token) = get_oauth_token() else {
        return;
    };
    let Some(usage) = fetch_usage(&token) else {
        return;
    };
    if let Ok(json) = serde_json::to_string(&usage) {
        let _ = std::fs::write(CACHE_PATH, json);
    }
}

fn format_statusline(
    model_name: &str,
    context_percentage: f64,
    context_window_size: u64,
    session_duration: &str,
    token_metrics: &str,
    usage: Option<&UsageResponse>,
    plain: bool,
) -> String {
    let mut components = Vec::with_capacity(8);

    let separator_color = if plain { "" } else { SEPARATOR_COLOR };
    let icon_color = if plain { "" } else { ICON_COLOR };
    let input_token_color = if plain { "" } else { INPUT_TOKEN_COLOR };
    let output_token_color = if plain { "" } else { OUTPUT_TOKEN_COLOR };
    let reset = if plain { "" } else { RESET };

    components.push(format_gradient_text(model_name, plain));

    let compaction_pct = autocompact_budget(context_window_size)
        .map(|budget| context_percentage * context_window_size as f64 / budget as f64);
    let context_color = get_context_color(compaction_pct.unwrap_or(context_percentage), plain);
    let mut context_str = format!("{}{}%", context_color, context_percentage as u64);
    if let Some(pct) = compaction_pct {
        if pct >= AUTOCOMPACT_WARN_PCT {
            let warn_color = get_context_color(pct, plain);
            context_str.push_str(&format!(" {}\u{2192}{}%", warn_color, pct as u64));
        }
    }
    components.push(context_str);

    let duration_minutes = parse_duration_to_minutes(session_duration);
    let duration_color = get_session_duration_color(duration_minutes, plain);
    components.push(format!("{}{}", duration_color, session_duration));

    let metrics_parts: Vec<&str> = token_metrics.split('/').collect();
    if metrics_parts.len() == 2 {
        components.push(format!(
            "{}{}/{}{}",
            input_token_color, metrics_parts[0], output_token_color, metrics_parts[1]
        ));
    }

    if let Some(usage) = usage {
        let five_hour_pct = usage
            .five_hour
            .as_ref()
            .and_then(|w| w.utilization)
            .map(|u| u.round() as u64);
        let seven_day_pct = usage
            .seven_day
            .as_ref()
            .and_then(|w| w.utilization)
            .map(|u| u.round() as u64);
        let five_hour_countdown = usage
            .five_hour
            .as_ref()
            .and_then(|w| w.resets_at.as_deref())
            .and_then(format_reset_countdown);
        let seven_day_countdown = usage
            .seven_day
            .as_ref()
            .and_then(|w| w.resets_at.as_deref())
            .and_then(format_reset_countdown);

        if five_hour_pct.is_some() || seven_day_pct.is_some() {
            let mut rate_parts = String::new();

            if let Some(pct) = five_hour_pct {
                let color = get_usage_color(pct as f64, plain);
                rate_parts.push_str(&format!("{}5h:{}{}%", icon_color, color, pct));
                if let Some(countdown) = five_hour_countdown {
                    rate_parts.push_str(&format!("{} ({})", icon_color, countdown));
                }
            }

            if let Some(pct) = seven_day_pct {
                if !rate_parts.is_empty() {
                    rate_parts.push_str(&format!(" {}\u{276F} ", separator_color));
                }
                let color = get_usage_color(pct as f64, plain);
                rate_parts.push_str(&format!("{}7d:{}{}%", icon_color, color, pct));
                if let Some(countdown) = seven_day_countdown {
                    rate_parts.push_str(&format!("{} ({})", icon_color, countdown));
                }
            }

            components.push(rate_parts);
        }
    }

    let joined = components.join(&format!(" {}\u{276F} ", separator_color));
    format!("{}{}", joined, reset)
}
