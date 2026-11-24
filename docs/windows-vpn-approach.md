# Windows VPN approach — Minab Windows

How Minab Windows connects to a VPN on Windows, and what it deliberately does **not** do.

## 1. The app does NOT implement OpenVPN

Minab Windows contains no VPN protocol code. It bundles or locates the **official OpenVPN 2.x `openvpn.exe`** (GPLv2, from [openvpn.net](https://openvpn.net)) and drives it as a child process over its **management interface**:

- The app starts `openvpn.exe` with:
  - `--management 127.0.0.1 <port>` — a local TCP management channel on loopback only (never exposed on the network)
  - `--management-query-passwords` — the app supplies the profile username/password over the management channel when OpenVPN asks for them
  - `--management-hold` — OpenVPN starts held, so the app can attach to the management channel before any traffic flows
- Over that channel the app uses:
  - `state` — connection state changes
  - `bytecount` — traffic counters (drives the Data used display)
  - `log` — OpenVPN's log output, shown in the app
  - `hold release` — releases the hold and lets the connection start
  - `signal SIGTERM` — clean disconnect (killing the process is the last resort, not the normal path)

If the management channel is unreachable, the connection is treated as failed — the app never guesses the state from the process being alive.

## 2. TAP-Windows6 driver

`openvpn.exe` needs a virtual network adapter — the **TAP-Windows6** driver — to carry traffic. Consequences:

- **v0.1.0:** the app locates an **existing OpenVPN install** (its install directory, from the registry / default `C:\Program Files\OpenVPN`) and uses that `openvpn.exe` and its already-installed TAP adapter. If no OpenVPN install is found, the app cannot connect and must say so plainly.
- **Later pass:** the installer must **bundle `openvpn.exe` and install the TAP driver itself** (admin install). Until then, the README's install step is: install OpenVPN 2.x from openvpn.net first.

## 3. Admin rights — the v0.1.0 decision

Connecting needs elevation, because creating/configuring the TAP adapter is a privileged operation. The options were:

1. Run the whole app as administrator.
2. Launch only the `openvpn.exe` child elevated (PowerShell `Start-Process -Verb RunAs`) when the app itself is not elevated.
3. A privileged helper service installed once, which the unprivileged app talks to.

**Decision for v0.1.0: option 1 only — run the app as administrator.** If the app is not elevated, `connect()` fails with a clear error telling the user to run Minab as administrator. **Option 2 (elevated `openvpn.exe` child via `Start-Process -Verb RunAs`) is a later pass** — the child would then be owned by PowerShell, so wait/kill semantics change and only the management-interface `signal SIGTERM` path stays reliable. **Option 3, a privileged helper service, is explicitly a later pass** — it is more moving parts, more install surface, and more to get wrong in a first release. The app must never silently fail when elevation is refused: it reports "connection needs administrator rights".

## 4. Profile storage

- Profiles (`.ovpn` contents, group, settings) are stored as **JSON** in `%APPDATA%/com.minab.children.windows` (the Tauri app-data directory for this app id).
- **Passwords in v0.1.0 are stored in plaintext in that JSON** — the same limitation as the Android app's full-backup file, which also holds passwords in plain text. This is a known, documented weakness, not an oversight.
- **Planned fix:** store passwords in **Windows Credential Manager** (via the `keyring` / Windows Credential API) instead of the JSON file, keeping only a reference in the profile. That is a later pass, not v0.1.0.

## 5. What is NOT done yet

- Bundling `openvpn.exe` and the TAP driver in the installer (v0.1.0 needs an existing OpenVPN install).
- Auto-failover to the next healthy server on disconnect (Android has it; Windows does not yet).
- Tray icon (minimize-to-tray / tray menu).
- Code signing — v0.1.0 installers are unsigned, so Windows SmartScreen will warn on install. Signing needs a certificate and a signing step in `.github/workflows/build-windows.yml`; it is a later pass.
- Credential Manager password storage (see §4).
- Privileged helper service (see §3).
