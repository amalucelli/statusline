// The line itself: which components appear, in what order, and in what colour.
//
// Each component encodes its severity as a step on a 256-colour ramp rather
// than in words. `plain` drops every escape sequence, and a component with
// nothing to report is left out rather than rendered empty.

use crate::time::{format_reset_countdown, parse_duration_to_minutes};
use crate::usage::UsageResponse;

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

const AUTOCOMPACT_WARN_PCT: f64 = 70.0;

// Auto-compact fires before the window fills: Claude Code holds back 20k tokens
// for the model output and 13k more as the compaction buffer. The statusline
// JSON reports used_percentage against the full window, so a 200k session
// compacts near 84% and a 1M session near 97%. /autocompact can move the
// window and the real threshold is not in the JSON.
const AUTOCOMPACT_RESERVE_TOKENS: u64 = 33_000;

fn autocompact_budget(context_window_size: u64) -> Option<u64> {
    context_window_size.checked_sub(AUTOCOMPACT_RESERVE_TOKENS)
}

pub fn format_token_value(tokens: u64) -> String {
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
        let color_idx = i
            .checked_div(segment_size)
            .unwrap_or(0)
            .min(GRADIENT_COLORS.len() - 1);
        result.push_str(GRADIENT_COLORS[color_idx]);
        result.push(*ch);
    }
    result
}

pub fn format_statusline(
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
