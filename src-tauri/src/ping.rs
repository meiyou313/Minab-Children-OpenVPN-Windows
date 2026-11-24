//! Latency probing, same semantics as the Android app:
//!
//! 1. ICMP first, via the system `ping` command
//!    (Windows: `ping -n 1 -w 2000 <host>`, parsed from `time=XXms` /
//!    `time<1ms`; Linux, for local checking only: `ping -c 1 -W 2`).
//! 2. If ICMP fails (many servers block it), fall back to a TCP connect to
//!    the server's own port with a 2 s timeout and measure the handshake.
//!
//! A good ping does not guarantee the server passes the filter once
//! connected, and a failed ping does not mean the server is unusable —
//! exactly as on Android.

use serde::{Deserialize, Serialize};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One ping-test target.
#[derive(Debug, Clone)]
pub struct PingTarget {
    pub profile_id: String,
    pub host: String,
    pub port: u16,
    /// Kept for future use (e.g. per-proto display); the probe itself is
    /// ICMP-first + TCP fallback regardless of proto, like Android.
    #[allow(dead_code)]
    pub proto: String,
}

/// One ping-test result (also the `ping-result` event payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResult {
    pub profile_id: String,
    pub ms: Option<u32>,
    pub ok: bool,
}

/// Probe one host: ICMP first, TCP-connect fallback. Returns latency in ms.
pub fn ping_one(host: &str, port: u16, _proto: &str) -> Option<u32> {
    if host.is_empty() {
        return None;
    }
    if let Some(ms) = icmp_ping(host) {
        return Some(ms);
    }
    tcp_ping(host, port)
}

fn icmp_ping(host: &str) -> Option<u32> {
    let output = run_ping_cmd(host)?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_ping_output(&text)
}

#[cfg(target_os = "windows")]
fn run_ping_cmd(host: &str) -> Option<std::process::Output> {
    Command::new("ping")
        .args(["-n", "1", "-w", "2000", host])
        .output()
        .ok()
}

#[cfg(not(target_os = "windows"))]
fn run_ping_cmd(host: &str) -> Option<std::process::Output> {
    Command::new("ping")
        .args(["-c", "1", "-W", "2", host])
        .output()
        .ok()
}

/// Parse `time=12ms`, `time<1ms` (Windows) and `time=0.123 ms` (Linux).
/// Returns `Some(0)` for `time<1ms`.
fn parse_ping_output(text: &str) -> Option<u32> {
    if text.contains("time<") {
        return Some(0);
    }
    let idx = text.find("time=")?;
    let rest = &text[idx + 5..];
    let mut num = String::new();
    for ch in rest.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
        } else {
            break;
        }
    }
    if num.is_empty() {
        return None;
    }
    num.parse::<f64>().ok().map(|v| v.round() as u32)
}

fn tcp_ping(host: &str, port: u16) -> Option<u32> {
    let addrs: Vec<std::net::SocketAddr> = (host, port).to_socket_addrs().ok()?.collect();
    let addr = *addrs.first()?;
    let start = Instant::now();
    match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(stream) => {
            drop(stream);
            Some(start.elapsed().as_millis() as u32)
        }
        Err(_) => None,
    }
}

/// Run one thread per target, bounded to 10 concurrent threads, and call
/// `on_result` for each finished probe (from the worker thread).
pub fn start_ping_test<F>(targets: Vec<PingTarget>, on_result: F)
where
    F: Fn(PingResult) + Send + Sync + 'static,
{
    let on_result = Arc::new(on_result);
    for chunk in targets.chunks(10) {
        let mut handles = Vec::new();
        for target in chunk {
            let cb = Arc::clone(&on_result);
            let target = target.clone();
            handles.push(std::thread::spawn(move || {
                let ms = ping_one(&target.host, target.port, &target.proto);
                let ok = ms.is_some();
                cb(PingResult {
                    profile_id: target.profile_id,
                    ms,
                    ok,
                });
            }));
        }
        for handle in handles {
            let _ = handle.join();
        }
    }
}
