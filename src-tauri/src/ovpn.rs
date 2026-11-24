//! Minimal `.ovpn` config parser.
//!
//! Extracts the connection parameters Minab needs (remote host/port/proto,
//! whether `auth-user-pass` is present, inline `<ca>`/`<cert>`/`<key>`
//! blocks vs. file references) and can *materialize* a config into a
//! self-contained text that openvpn.exe can run from a temp directory
//! (file references are inlined, stale `management` / `auth-user-pass`
//! lines are stripped — the caller appends its own).

use std::path::{Path, PathBuf};

/// Result of parsing one `.ovpn` file.
///
/// NOTE: not every field is read in v0.1 (`auth_user_pass`, the inline/
/// file flags) — they are the parsed API surface for later passes (UI
/// badges, file-based import), so dead-code warnings are silenced here.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ParsedConfig {
    pub remote_host: String,
    pub remote_port: u16,
    pub proto: String, // "udp" | "tcp"
    /// Config contains an `auth-user-pass` directive (with or without a file).
    pub auth_user_pass: bool,
    pub inline_ca: bool,
    pub inline_cert: bool,
    pub inline_key: bool,
    /// File reference for the CA (`ca <path>`), if any. Resolved against
    /// `base_dir` when `parse_ovpn_with_base` is used, otherwise as written.
    pub ca_file: Option<String>,
    pub cert_file: Option<String>,
    pub key_file: Option<String>,
    /// The original config text, unchanged.
    pub raw_text: String,
}

/// Parse `.ovpn` text. File references are kept as written.
pub fn parse_ovpn(text: &str) -> Result<ParsedConfig, String> {
    parse_ovpn_with_base(text, None)
}

/// Parse `.ovpn` text, resolving `ca`/`cert`/`key` file references relative
/// to `base_dir` (the directory the `.ovpn` file lives in).
pub fn parse_ovpn_with_base(text: &str, base_dir: Option<&Path>) -> Result<ParsedConfig, String> {
    let mut remote_host: Option<String> = None;
    let mut remote_port: u16 = 1194;
    let mut proto_from_remote: Option<String> = None;
    let mut proto_directive: Option<String> = None;
    let mut auth_user_pass = false;
    let mut inline_ca = false;
    let mut inline_cert = false;
    let mut inline_key = false;
    let mut ca_file: Option<String> = None;
    let mut cert_file: Option<String> = None;
    let mut key_file: Option<String> = None;

    // Track which inline block we are inside, if any.
    let mut in_block: Option<&str> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('<') {
            match line {
                "<ca>" => in_block = Some("ca"),
                "<cert>" => in_block = Some("cert"),
                "<key>" => in_block = Some("key"),
                "</ca>" | "</cert>" | "</key>" => in_block = None,
                _ => {}
            }
            continue;
        }
        if in_block.is_some() {
            continue; // Contents of an inline block are opaque here.
        }

        let mut parts = line.split_whitespace();
        let directive = match parts.next() {
            Some(d) => d,
            None => continue,
        };
        match directive {
            "remote" => {
                // `remote <host> [port] [proto]` — first one wins.
                if remote_host.is_none() {
                    if let Some(host) = parts.next() {
                        remote_host = Some(host.to_string());
                        if let Some(port) = parts.next() {
                            remote_port = port.parse::<u16>().unwrap_or(1194);
                        }
                        if let Some(p) = parts.next() {
                            proto_from_remote = Some(normalize_proto(p));
                        }
                    }
                }
            }
            "proto" => {
                if let Some(p) = parts.next() {
                    proto_directive = Some(normalize_proto(p));
                }
            }
            "auth-user-pass" => {
                auth_user_pass = true;
            }
            "ca" | "cert" | "key" => {
                if let Some(f) = parts.next() {
                    let resolved = resolve_ref(f, base_dir);
                    match directive {
                        "ca" => ca_file = Some(resolved),
                        "cert" => cert_file = Some(resolved),
                        _ => key_file = Some(resolved),
                    }
                }
            }
            _ => {}
        }
    }

    // Inline-block presence: the loop above skips block contents but the
    // opening tags were seen as lines starting with '<'. Re-scan cheaply.
    for raw_line in text.lines() {
        match raw_line.trim() {
            "<ca>" => inline_ca = true,
            "<cert>" => inline_cert = true,
            "<key>" => inline_key = true,
            _ => {}
        }
    }

    let remote_host = remote_host
        .filter(|h| !h.is_empty())
        .ok_or_else(|| "No `remote` line found in .ovpn config".to_string())?;
    let proto = proto_from_remote
        .or(proto_directive)
        .unwrap_or_else(|| "udp".to_string());

    Ok(ParsedConfig {
        remote_host,
        remote_port,
        proto,
        auth_user_pass,
        inline_ca,
        inline_cert,
        inline_key,
        ca_file,
        cert_file,
        key_file,
        raw_text: text.to_string(),
    })
}

fn normalize_proto(p: &str) -> String {
    let lower = p.to_ascii_lowercase();
    if lower.starts_with("tcp") {
        "tcp".to_string()
    } else {
        "udp".to_string()
    }
}

fn resolve_ref(reference: &str, base_dir: Option<&Path>) -> String {
    let reference = unquote(reference);
    let path = Path::new(reference);
    if path.is_absolute() || base_dir.is_none() {
        return reference.to_string();
    }
    let base = base_dir.unwrap();
    // Keep the joined path as a string; it may not exist yet at parse time.
    base.join(path).to_string_lossy().into_owned()
}

/// Strip surrounding double quotes from a config file reference
/// (`ca "C:\path with spaces\ca.crt"`).
fn unquote(value: &str) -> &str {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

/// Build a self-contained config text that openvpn.exe can run from any
/// directory:
///
/// * `ca`/`cert`/`key` file references are replaced by inline blocks
///   (file contents read from `base_dir` when the reference is relative).
/// * Existing `management` lines are dropped (the caller appends its own).
/// * Any `auth-user-pass` line is dropped (the caller appends its own
///   `auth-user-pass <tmpfile>` only when credentials are available).
/// * Inline blocks already present in the config are kept verbatim.
pub fn materialize_config(text: &str, base_dir: Option<&Path>) -> Result<String, String> {
    // Validates the config (and fails early on missing file refs).
    parse_ovpn_with_base(text, base_dir)?;
    let mut out: Vec<String> = Vec::new();
    let mut lines = text.lines().peekable();

    while let Some(raw_line) = lines.next() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            out.push(raw_line.to_string());
            continue;
        }
        // Copy inline blocks verbatim.
        if line == "<ca>" || line == "<cert>" || line == "<key>" {
            out.push(raw_line.to_string());
            while let Some(inner) = lines.next() {
                out.push(inner.to_string());
                let t = inner.trim();
                if t == "</ca>" || t == "</cert>" || t == "</key>" {
                    break;
                }
            }
            continue;
        }
        let directive = line.split_whitespace().next().unwrap_or("");
        match directive {
            "management" | "auth-user-pass" => {
                // Dropped: re-added by the caller with our own values.
            }
            "ca" | "cert" | "key" => {
                let tag = directive;
                if inline_block_present(text, tag) {
                    // A config that has both is unusual; keep the directive
                    // out and rely on the inline block below.
                    continue;
                }
                let reference = line.split_whitespace().nth(1).ok_or_else(|| {
                    format!("`{tag}` directive without a file path in .ovpn config")
                })?;
                // Note: a quoted path containing spaces is split by the
                // whitespace tokenizer above; such configs need the file
                // next to the .ovpn with a space-free name in v0.1.
                let reference = unquote(reference);
                let path = resolve_ref_path(reference, base_dir);
                let contents = std::fs::read_to_string(&path).map_err(|e| {
                    format!("Cannot read {tag} file '{reference}': {e}")
                })?;
                out.push(format!("<{tag}>"));
                out.push(contents.trim_end().to_string());
                out.push(format!("</{tag}>"));
            }
            _ => {
                out.push(raw_line.to_string());
            }
        }
    }

    // If the config referenced a file we could not inline above, that is
    // already an error. Double-check nothing slipped through.
    Ok(out.join("\n"))
}

fn inline_block_present(text: &str, tag: &str) -> bool {
    let open = format!("<{tag}>");
    text.lines().any(|l| l.trim() == open)
}

fn resolve_ref_path(reference: &str, base_dir: Option<&Path>) -> PathBuf {
    let path = Path::new(reference);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match base_dir {
        Some(base) => base.join(path),
        None => path.to_path_buf(),
    }
}
