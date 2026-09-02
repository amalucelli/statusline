// Durations rendered short, and an RFC 3339 reset instant turned into a
// countdown. A timestamp that does not parse yields `None` rather than an
// error, and the caller drops the component.

use std::time::SystemTime;

pub fn format_duration_from_ms(ms: u64) -> String {
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

pub fn format_reset_countdown(resets_at: &str) -> Option<String> {
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

pub fn parse_duration_to_minutes(duration_str: &str) -> u64 {
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
