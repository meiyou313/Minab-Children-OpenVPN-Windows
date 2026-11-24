# Minab Windows

**Minab Windows** is the Windows companion to the Android [Minab OpenVPN client](https://github.com/meiyou313/Minab-Children-OpenVPN) — an OpenVPN client built in memory of the 168 children of Minab Elementary School.

It brings the same idea to the desktop: import `.ovpn` profiles, group them, ping-test them, and connect — with a UI that mirrors the Android app.

## Stack

- **Tauri v2 + Rust** — the app shell and backend (`src-tauri/`)
- **Vanilla HTML / CSS / JS** — the frontend (`src/`)
- **Official OpenVPN 2.x** — the app does **not** implement the OpenVPN protocol. It drives the official `openvpn.exe` (from [openvpn.net](https://openvpn.net)) over its management interface. See [docs/windows-vpn-approach.md](docs/windows-vpn-approach.md).

## Status — v0.1.0 scaffold

- Import `.ovpn` profiles, connect / disconnect, ping-test profiles.
- UI mirrors the Android app (profiles, groups, ping, connect).
- Not yet done: bundling `openvpn.exe` and the TAP driver in the installer, auto-failover, tray icon, code signing. See [docs/windows-vpn-approach.md](docs/windows-vpn-approach.md#what-is-not-done-yet).

## Build on Windows

Prerequisites: **Rust stable** (via [rustup](https://rustup.rs)) and **Node.js 22**.

```powershell
npm install
npm run tauri build
```

The installer lands in `src-tauri/target/release/bundle/` (`.msi` and/or NSIS `.exe`, depending on `src-tauri/tauri.conf.json`), and the raw binary in `src-tauri/target/release/minab-windows.exe`.

No Windows SDK is needed locally for anything else — CI builds on GitHub-hosted `windows-latest` runners (see [.github/workflows/build-windows.yml](.github/workflows/build-windows.yml)). Every push to a `minab-windows-*` branch or `v*` tag builds there; pushing a `v*` tag also publishes the installer as a GitHub pre-release.

## Connecting needs administrator rights

OpenVPN needs to create a TAP network adapter, which requires elevation. For v0.1.0, **run the app as administrator** — connecting fails with a clear error otherwise. Launching only the `openvpn.exe` child elevated (PowerShell `Start-Process -Verb RunAs`) and a privileged helper service are later passes. Details: [docs/windows-vpn-approach.md](docs/windows-vpn-approach.md).

## The OpenVPN / TAP approach, in short

Minab Windows never speaks the VPN protocol itself. It finds the official OpenVPN 2.x `openvpn.exe` (an existing install, or one bundled with the app later), starts it as a child process with `--management 127.0.0.1 <port>` plus `--management-query-passwords` and `--management-hold`, and drives it over that local TCP management channel: `state` and `bytecount` for status and traffic, `log` for output, `hold release` to start, and `signal SIGTERM` to stop. `openvpn.exe` needs the TAP-Windows6 adapter, which the installer must install (admin) — in v0.1.0 an existing OpenVPN install provides it.

## Android version

- Android app (the original): https://github.com/meiyou313/Minab-Children-OpenVPN
- The Android app is native Java/Kotlin on ics-openvpn (GPLv2). This Windows version is a separate codebase (Tauri v2 + Rust) with the same purpose.

## License

GPL-2.0 — see [LICENSE](LICENSE). The app drives the GPLv2 OpenVPN binary, and its source stays open.
