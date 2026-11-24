//! Windows OpenVPN driver — drives the OFFICIAL OpenVPN 2.x `openvpn.exe`
//! through its management interface. The VPN protocol itself is never
//! reimplemented here.
//!
//! WINDOWS / ADMIN RIGHTS — READ FIRST (v0.1 approach):
//! `openvpn.exe` needs administrator rights to create/configure the TAP
//! adapter (and to add routes). v0.1 therefore assumes the app — or at
//! least the `openvpn.exe` child process — runs elevated. Two supported
//! ways to get there:
//!   1. Run the app itself as administrator (right-click -> "Run as
//!      administrator", or set it in the shortcut / installer).
//!   2. Launch `openvpn.exe` elevated via PowerShell
//!      `Start-Process -Verb RunAs` when the app itself is not elevated
//!      (this pops a UAC prompt for the VPN only). NOT implemented in
//!      v0.1: the child would then be owned by PowerShell, so wait/kill
//!      semantics change — only the management-interface `signal SIGTERM`
//!      path stays reliable. A privileged helper service is the cleaner
//!      long-term answer and is a later pass.
//! Until one of those is in place, `connect()` fails with a clear error
//! telling the user to run the app as administrator.
//!
//! How it works:
//! * `connect()` writes a temp `.ovpn` (materialized config + management
//!   lines), a temp auth file and a temp log file, spawns `openvpn.exe`
//!   with `--management 127.0.0.1 <free_port> --management-query-passwords
//!   --management-hold --auth-user-pass <tmp_auth> --verb 3 --log <tmp_log>`,
//!   connects a `TcpStream` to the management port, sends `state on`,
//!   `bytecount 2`, `log on`, `hold release`, and spawns a reader thread.
//! * The reader thread parses management lines:
//!   `>STATE:<ts>,<STATE>,...` -> connection state,
//!   `>BYTECOUNT:<in>,<out>`   -> byte counters (emitted, and fed to the
//!   usage store by lib.rs via the event sink),
//!   `>PASSWORD:Need 'Auth' username/password` -> sends the credentials
//!   over the management channel,
//!   `>PASSWORD:Verification Failed ...` -> Error state.
//! * `disconnect()` sends `signal SIGTERM`, waits up to 5 s, then kills
//!   the child if it is still alive.

use crate::ovpn;
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Connection state tracked by the manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VpnState {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

impl VpnState {
    pub fn as_str(&self) -> &'static str {
        match self {
            VpnState::Disconnected => "disconnected",
            VpnState::Connecting => "connecting",
            VpnState::Connected => "connected",
            VpnState::Error => "error",
        }
    }
}

/// Events the manager emits through its sink (wired to Tauri events by lib.rs).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum VpnEvent {
    StateChanged { state: VpnState, message: String },
    ByteCount { bytes_in: u64, bytes_out: u64 },
}

/// Callback type lib.rs installs so manager events reach the frontend.
pub type EventSink = Arc<dyn Fn(VpnEvent) + Send + Sync>;

struct Inner {
    state: VpnState,
    profile_id: Option<String>,
    bytes_in: u64,
    bytes_out: u64,
    message: String,
    child: Option<Child>,
    /// Clone of the management socket used for *writing* commands.
    /// The reader thread owns the original stream.
    mgmt_writer: Option<Arc<Mutex<TcpStream>>>,
    auth_user: Option<String>,
    auth_pass: Option<String>,
    /// Temp files for the current session (kept so disconnect can clean up).
    tmp_dir: Option<PathBuf>,
}

impl Inner {
    fn fresh() -> Self {
        Self {
            state: VpnState::Disconnected,
            profile_id: None,
            bytes_in: 0,
            bytes_out: 0,
            message: "Disconnected".to_string(),
            child: None,
            mgmt_writer: None,
            auth_user: None,
            auth_pass: None,
            tmp_dir: None,
        }
    }
}

/// Thread-safe OpenVPN process manager. Cheap to clone; all mutable state
/// lives behind one `Arc<Mutex<...>>` so the management reader thread can
/// update it.
#[derive(Clone)]
pub struct OpenVpnManager {
    inner: Arc<Mutex<Inner>>,
    sink: Arc<Mutex<Option<EventSink>>>,
}

impl Default for OpenVpnManager {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::fresh())),
            sink: Arc::new(Mutex::new(None)),
        }
    }
}

impl OpenVpnManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the event sink (called once from lib.rs `setup`).
    pub fn set_sink(&self, sink: EventSink) {
        if let Ok(mut slot) = self.sink.lock() {
            *slot = Some(sink);
        }
    }

    fn emit(&self, event: VpnEvent) {
        let sink = match self.sink.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => return,
        };
        if let Some(sink) = sink {
            sink(event);
        }
    }

    /// Snapshot for `get_status()`.
    pub fn status_snapshot(&self) -> (VpnState, Option<String>, u64, u64, String) {
        match self.inner.lock() {
            Ok(inner) => (
                inner.state,
                inner.profile_id.clone(),
                inner.bytes_in,
                inner.bytes_out,
                inner.message.clone(),
            ),
            Err(_) => (
                VpnState::Error,
                None,
                0,
                0,
                "Internal state lock poisoned".to_string(),
            ),
        }
    }

    /// Locate `openvpn.exe`, in order:
    /// 1. Sidecar resources dir (`resources/openvpn/bin/openvpn.exe` under
    ///    the Tauri resource dir, when provided).
    /// 2. `%ProgramFiles%\OpenVPN\bin\openvpn.exe`
    ///    (and `%ProgramFiles(x86)%`).
    /// 3. Any `openvpn.exe` on `PATH`.
    pub fn locate_openvpn(resource_dir: Option<PathBuf>) -> Result<PathBuf, String> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(dir) = resource_dir {
            candidates.push(
                dir.join("openvpn")
                    .join("bin")
                    .join("openvpn.exe"),
            );
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(
                    dir.join("resources")
                        .join("openvpn")
                        .join("bin")
                        .join("openvpn.exe"),
                );
            }
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            candidates.push(
                PathBuf::from(pf)
                    .join("OpenVPN")
                    .join("bin")
                    .join("openvpn.exe"),
            );
        }
        if let Ok(pf) = std::env::var("ProgramFiles(x86)") {
            candidates.push(
                PathBuf::from(pf)
                    .join("OpenVPN")
                    .join("bin")
                    .join("openvpn.exe"),
            );
        }
        for candidate in &candidates {
            if candidate.is_file() {
                return Ok(candidate.clone());
            }
        }
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join("openvpn.exe");
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        Err("openvpn.exe not found. Install the official OpenVPN client \
             (openvpn.net) or bundle openvpn.exe under resources/openvpn/bin/."
            .to_string())
    }

    /// Connect to a profile. Returns once the management interface is up
    /// and `hold release` has been sent; the actual tunnel comes up
    /// asynchronously and is reported via `vpn-state` events.
    pub fn connect(
        &self,
        profile_id: &str,
        profile_name: &str,
        config_text: &str,
        username: Option<&str>,
        password: Option<&str>,
        resource_dir: Option<PathBuf>,
    ) -> Result<(), String> {
        // Any previous session is torn down first.
        let _ = self.disconnect();

        let exe = Self::locate_openvpn(resource_dir)?;

        // Pick a free localhost management port.
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("Cannot reserve a management port: {e}"))?;
        let mgmt_port = listener.local_addr().map_err(|e| e.to_string())?.port();
        drop(listener);

        // Temp working dir for this session.
        let tmp_dir = std::env::temp_dir()
            .join("minab-windows")
            .join(format!("session-{profile_id}"));
        std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
        let config_path = tmp_dir.join("config.ovpn");
        let auth_path = tmp_dir.join("auth.txt");
        let log_path = tmp_dir.join("openvpn.log");

        // Materialize the config (inline any ca/cert/key file refs) and
        // append our management + auth lines.
        let mut config = ovpn::materialize_config(config_text, None).map_err(|e| {
            format!("Cannot prepare config for '{profile_name}': {e}")
        })?;
        config.push_str(&format!(
            "\nmanagement 127.0.0.1 {mgmt_port}\nmanagement-query-passwords\nmanagement-hold\n"
        ));
        let has_creds = username.map(|u| !u.is_empty()).unwrap_or(false);
        if has_creds {
            config.push_str(&format!(
                "auth-user-pass \"{}\"\n",
                auth_path.to_string_lossy()
            ));
            let auth_contents = format!(
                "{}\n{}\n",
                username.unwrap_or(""),
                password.unwrap_or("")
            );
            std::fs::write(&auth_path, auth_contents).map_err(|e| e.to_string())?;
        }
        std::fs::write(&config_path, &config).map_err(|e| e.to_string())?;

        // Spawn openvpn.exe (no console window on Windows).
        // Owned Strings: passing temporaries (e.g. &path.to_string_lossy())
        // as args is fragile across AsRef<OsStr> impls.
        let config_arg = config_path.to_string_lossy().into_owned();
        let log_arg = log_path.to_string_lossy().into_owned();
        let auth_arg = auth_path.to_string_lossy().into_owned();
        let mut cmd = Command::new(&exe);
        cmd.args([
            "--config",
            &config_arg,
            "--management",
            "127.0.0.1",
            &mgmt_port.to_string(),
            "--management-query-passwords",
            "--management-hold",
            "--verb",
            "3",
            "--log",
            &log_arg,
        ]);
        if has_creds {
            cmd.args(["--auth-user-pass", &auth_arg]);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(target_os = "windows")]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().map_err(|e| {
            format!(
                "Cannot start openvpn.exe ({e}). openvpn.exe needs administrator \
                 rights for the TAP adapter — try running Minab as administrator."
            )
        })?;

        // Wait for the management interface to accept connections.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream: Option<TcpStream> = None;
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return Err(format!(
                        "openvpn.exe exited during startup (status {status}). \
                         See the log at {}",
                        log_path.to_string_lossy()
                    ));
                }
                Ok(None) => {}
                Err(e) => return Err(format!("Cannot check openvpn.exe: {e}")),
            }
            match TcpStream::connect(("127.0.0.1", mgmt_port)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
        let stream = stream.ok_or_else(|| {
            let _ = child.kill();
            format!(
                "Timed out waiting for the OpenVPN management interface. \
                 See the log at {}",
                log_path.to_string_lossy()
            )
        })?;
        let _ = stream.set_nodelay(true);

        let writer = Arc::new(Mutex::new(
            stream.try_clone().map_err(|e| e.to_string())?,
        ));
        let reader = BufReader::new(stream);

        {
            let mut inner = self.inner.lock().map_err(|_| "State lock poisoned")?;
            inner.state = VpnState::Connecting;
            inner.profile_id = Some(profile_id.to_string());
            inner.bytes_in = 0;
            inner.bytes_out = 0;
            inner.message = format!("Connecting to {profile_name}...");
            inner.child = Some(child);
            inner.mgmt_writer = Some(Arc::clone(&writer));
            inner.auth_user = username.map(|u| u.to_string());
            inner.auth_pass = password.map(|p| p.to_string());
            inner.tmp_dir = Some(tmp_dir);
        }

        // Initial management commands, then release the hold.
        self.send_mgmt("state on")?;
        self.send_mgmt("bytecount 2")?;
        self.send_mgmt("log on")?;
        self.send_mgmt("hold release")?;

        // Reader thread owns the management socket from here on.
        let manager = self.clone();
        std::thread::spawn(move || {
            manager.run_reader(reader, writer);
        });

        self.emit(VpnEvent::StateChanged {
            state: VpnState::Connecting,
            message: format!("Connecting to {profile_name}..."),
        });
        Ok(())
    }

    /// Send one line to the management interface (best effort on lock errors).
    fn send_mgmt(&self, line: &str) -> Result<(), String> {
        let writer = {
            let inner = self.inner.lock().map_err(|_| "State lock poisoned")?;
            match &inner.mgmt_writer {
                Some(w) => Arc::clone(w),
                None => return Err("Not connected to the management interface".into()),
            }
        };
        let mut guard = writer.lock().map_err(|_| "Management lock poisoned")?;
        guard
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|_| guard.flush())
            .map_err(|e| e.to_string())
    }

    fn run_reader(&self, reader: BufReader<TcpStream>, writer: Arc<Mutex<TcpStream>>) {
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if let Some(rest) = line.strip_prefix(">STATE:") {
                self.handle_state_line(rest);
            } else if let Some(rest) = line.strip_prefix(">BYTECOUNT:") {
                self.handle_bytecount_line(rest);
            } else if let Some(rest) = line.strip_prefix(">PASSWORD:") {
                self.handle_password_line(rest, &writer);
            } else if let Some(rest) = line.strip_prefix(">LOG:") {
                // >LOG:<ts>,<level>,<message> — keep the latest as the
                // status message, but do not spam events for every line.
                if let Ok(mut inner) = self.inner.lock() {
                    let msg = rest.splitn(3, ',').nth(2).unwrap_or(rest);
                    if !msg.trim().is_empty() && inner.state == VpnState::Connecting {
                        inner.message = truncate(msg.trim(), 200);
                    }
                }
            }
            // >INFO: (greeting), >HOLD:, >ECHO: and others are ignored.
        }

        // Management socket closed: the process exited (or was killed).
        let mut event: Option<VpnEvent> = None;
        if let Ok(mut inner) = self.inner.lock() {
            if inner.state != VpnState::Disconnected {
                inner.state = VpnState::Disconnected;
                inner.message = "OpenVPN process exited".to_string();
                inner.child = None;
                inner.mgmt_writer = None;
                event = Some(VpnEvent::StateChanged {
                    state: VpnState::Disconnected,
                    message: inner.message.clone(),
                });
            }
        }
        if let Some(event) = event {
            self.emit(event);
        }
    }

    fn handle_state_line(&self, rest: &str) {
        // >STATE:<ts>,<NAME>,<desc>,<local_ip>,...
        let parts: Vec<&str> = rest.split(',').collect();
        let name = parts.get(1).copied().unwrap_or("");
        let (state, message) = match name {
            "CONNECTED" => (VpnState::Connected, "Connected".to_string()),
            "CONNECTING" => (VpnState::Connecting, "Connecting...".to_string()),
            "RECONNECTING" => (VpnState::Connecting, "Reconnecting...".to_string()),
            "WAIT" => (VpnState::Connecting, "Waiting...".to_string()),
            "AUTH" => (VpnState::Connecting, "Authenticating...".to_string()),
            "GET_CONFIG" => (
                VpnState::Connecting,
                "Getting configuration...".to_string(),
            ),
            "ASSIGN_IP" => (
                VpnState::Connecting,
                "Assigning IP address...".to_string(),
            ),
            "ADD_ROUTES" => (VpnState::Connecting, "Adding routes...".to_string()),
            "EXITING" => (VpnState::Disconnected, "Exiting".to_string()),
            "" => return,
            other => (VpnState::Connecting, other.to_string()),
        };
        let changed = {
            let mut inner = match self.inner.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let changed = inner.state != state || inner.message != message;
            inner.state = state;
            inner.message = message.clone();
            changed
        };
        if changed {
            self.emit(VpnEvent::StateChanged { state, message });
        }
    }

    fn handle_bytecount_line(&self, rest: &str) {
        let mut parts = rest.split(',');
        let bytes_in: u64 = parts.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        let bytes_out: u64 = parts.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        if let Ok(mut inner) = self.inner.lock() {
            inner.bytes_in = bytes_in;
            inner.bytes_out = bytes_out;
        }
        self.emit(VpnEvent::ByteCount {
            bytes_in,
            bytes_out,
        });
    }

    fn handle_password_line(&self, rest: &str, writer: &Arc<Mutex<TcpStream>>) {
        if rest.contains("Verification Failed") {
            if let Ok(mut inner) = self.inner.lock() {
                inner.state = VpnState::Error;
                inner.message =
                    "Authentication failed — check the username and password".to_string();
            }
            self.emit(VpnEvent::StateChanged {
                state: VpnState::Error,
                message: "Authentication failed — check the username and password".to_string(),
            });
            return;
        }
        if rest.contains("Need") && rest.contains("'Auth'") {
            let (user, pass) = match self.inner.lock() {
                Ok(inner) => (inner.auth_user.clone(), inner.auth_pass.clone()),
                Err(_) => (None, None),
            };
            if let (Some(user), Some(pass)) = (user, pass) {
                if let Ok(mut guard) = writer.lock() {
                    let _ = guard.write_all(
                        format!(
                            "username \"Auth\" {}\npassword \"Auth\" {}\n",
                            escape_mgmt(&user),
                            escape_mgmt(&pass)
                        )
                        .as_bytes(),
                    );
                    let _ = guard.flush();
                }
            } else if let Ok(mut inner) = self.inner.lock() {
                inner.message = "Server asked for a username and password".to_string();
            }
        }
    }

    /// Disconnect: `signal SIGTERM` over management, wait up to 5 s, then
    /// kill the child if it is still alive. Safe to call when disconnected.
    pub fn disconnect(&self) -> Result<(), String> {
        let (child, writer, tmp_dir) = {
            let mut inner = self.inner.lock().map_err(|_| "State lock poisoned")?;
            let child = inner.child.take();
            let writer = inner.mgmt_writer.take();
            let tmp_dir = inner.tmp_dir.take();
            inner.state = VpnState::Disconnected;
            inner.message = "Disconnected".to_string();
            inner.profile_id = None;
            inner.bytes_in = 0;
            inner.bytes_out = 0;
            inner.auth_user = None;
            inner.auth_pass = None;
            (child, writer, tmp_dir)
        };

        if let Some(writer) = writer {
            if let Ok(mut guard) = writer.lock() {
                let _ = guard.write_all(b"signal SIGTERM\n");
                let _ = guard.flush();
            }
        }
        if let Some(mut child) = child {
            let start = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if start.elapsed() > Duration::from_secs(5) {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(_) => break,
                }
            }
        }
        // Remove the session temp dir (it holds the plaintext auth file).
        if let Some(dir) = tmp_dir {
            let _ = std::fs::remove_dir_all(dir);
        }

        self.emit(VpnEvent::StateChanged {
            state: VpnState::Disconnected,
            message: "Disconnected".to_string(),
        });
        Ok(())
    }
}

/// Escape a value embedded in a quoted management-interface argument.
fn escape_mgmt(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect()
    }
}
