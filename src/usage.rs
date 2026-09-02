// The account's rate-limit usage, and the cache that keeps fetching it off the
// hot path.
//
// Nothing here blocks the line: a stale cache is served as-is and the refresh is
// handed to a detached copy of the binary, guarded by a lock file. Every failure
// — no token, no network, a bad response — comes back as `None`.

use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io;
use std::process::Stdio;
use std::time::{Duration, SystemTime};

const CACHE_PATH: &str = "/tmp/claude-code-status-usage.json";
const REFRESH_LOCK_PATH: &str = "/tmp/claude-code-status-usage.refresh.lock";
const CACHE_TTL_EMPTY_SECS: u64 = 60;
const CACHE_TTL_FULL_SECS: u64 = 120;
const REFRESH_LOCK_STALE_SECS: u64 = 60;

#[derive(Debug, Deserialize, Serialize)]
pub struct UsageResponse {
    pub five_hour: Option<UsageWindow>,
    pub seven_day: Option<UsageWindow>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UsageWindow {
    pub utilization: Option<f64>,
    pub resets_at: Option<String>,
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

pub fn get_cached_usage() -> Option<UsageResponse> {
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

    // Spawning through the path on disk means an upgrade that lands mid-session
    // is picked up by the next refresh.
    let spawn_result = std::process::Command::new(exe)
        .arg(format!("--{}", crate::REFRESH_FLAG))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    if spawn_result.is_err() {
        let _ = std::fs::remove_file(lock_path);
    }
}

pub fn refresh_usage_cache() {
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
