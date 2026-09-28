<!-- Screenshots placeholder: when available, add images under docs/images/
     (e.g. docs/images/sender.png, docs/images/receiver.png) and embed them
     here as markdown images. -->

# Wireless Display Tool

**Mirror your Mac or Windows screen to any Android TV — peer-to-peer over your
local network. No cloud, no account, no dongle.**

[![CI](https://github.com/tsanys/wireless-display-tool/actions/workflows/ci.yml/badge.svg)](https://github.com/tsanys/wireless-display-tool/actions/workflows/ci.yml)
[![Release](https://github.com/tsanys/wireless-display-tool/actions/workflows/release.yml/badge.svg)](https://github.com/tsanys/wireless-display-tool/actions/workflows/release.yml)
[![Latest release](https://img.shields.io/github/v/release/tsanys/wireless-display-tool?include_prereleases&label=release)](https://github.com/tsanys/wireless-display-tool/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Android%20TV-blue)

Wireless Display Tool (WDT) streams your laptop's screen to an Android TV /
Google TV over Wi-Fi or Ethernet. Video and audio travel **directly between the
laptop and the TV** (WebRTC P2P, hardware H.264 encode/decode); a small
signaling server is embedded in the sender app, so there is nothing to deploy
and nothing leaves your LAN.

- **Sender** — desktop app for **macOS** and **Windows** (Tauri: Rust backend + web UI).
- **Receiver** — native **Android TV / Google TV** app (Kotlin + libwebrtc).
- **Core** — shared Rust crate: screen capture, hardware encoding, system audio, signaling.

> **Project status: early / v0.1.0.** The macOS path is runtime-validated
> end-to-end; Windows is implemented and compile-verified but **not yet
> runtime-tested on real hardware**. Releases are currently **unsigned**. See
> [Status & roadmap](#status--roadmap) for the honest details.

---

## Features

| Area | What you get |
|---|---|
| **Video** | 1080p / 720p, 30 / 60 fps, quality presets **Balanced** (10 Mbps cap) and **Sharp (text)** (18 Mbps cap). Chosen in the sender UI and remembered per TV. |
| **Audio** | System audio routed as Opus/WebRTC with four modes: **Laptop** (local only), **TV**, **Both**, **Muted**. |
| **Discovery** | Automatic mDNS/NSD discovery, plus a manual fallback: type `ip:port:token` on the TV or scan the QR shown in the sender. |
| **Pairing** | A 6-digit pairing token is generated per session and is never advertised over mDNS. |
| **Localization** | Full **English / Indonesian** UI on both sender and receiver (follows the system language). |
| **Low latency** | Optional receiver toggle (experimental) that trims the WebRTC jitter buffer. |
| **Device awareness** | The receiver classifies the TV (RAM + hardware H.264 decoder) and reports a capability tier. |
| **Diagnostics** | One-click export of a local diagnostics bundle (technical state only — **no token, no IP, no credentials**, never uploaded). |
| **Extended display** | Experimental on macOS (virtual display); capability-gated until the backend is complete. |

---

## Architecture

```text
┌─────────────────────────┐        mDNS discovery         ┌──────────────────────┐
│   Sender App (Tauri)     │ ◄────────────────────────────► │  Receiver App (TV)   │
│   macOS / Windows         │                                │  Kotlin + libwebrtc  │
│                           │      signaling (WebSocket)     │                      │
│  ┌─────────────────────┐ │ ◄──────────────────────────────┼─► signaling client   │
│  │ Rust core            │ │      (server embedded here)    │                      │
│  │ · screen capture     │ │                                │  ┌────────────────┐  │
│  │ · H.264 hardware enc │ │    media (WebRTC P2P / SRTP)   │  │ HW decode      │  │
│  │ · system audio (Opus)│ │ ◄──────────────────────────────┼─►│ + render       │  │
│  │ · signaling server   │ │                                │  └────────────────┘  │
│  └─────────────────────┘ │                                └──────────────────────┘
└─────────────────────────┘
```

Connection flow:

1. Sender opens → embedded signaling server binds on the LAN and advertises `_wdt._tcp` via mDNS.
2. Receiver discovers the sender (NSD) or you enter `ip:port:token` manually → WebSocket connects.
3. SDP offer/answer and ICE candidates are exchanged → a direct **P2P WebRTC** session forms.
4. The core captures the screen → hardware H.264 → WebRTC media track.
5. The TV hardware-decodes and renders fullscreen; audio plays as a synced Opus track.

No TURN/relay and no external server are required on a normal home/office LAN.

---

## Download & install

Grab the latest from the [Releases page](https://github.com/tsanys/wireless-display-tool/releases):

| Platform | Artifact | Install |
|---|---|---|
| macOS (Intel + Apple Silicon) | `wdt-sender-<tag>-macos-universal.dmg` | Open the `.dmg`, drag to Applications. |
| Windows x64 | `wdt-sender-<tag>-windows-x64-setup.exe` | Run the installer (NSIS). |
| Android TV / Google TV | `wdt-receiver-<tag>-android.apk` | `adb install -r <apk>` (sideload). |

> **Artifacts are currently unsigned (adhoc).**
> - **macOS:** Gatekeeper will warn → right-click the app → **Open**, or run
>   `xattr -dr com.apple.quarantine "/Applications/Wireless Display Sender.app"`.
> - **Windows:** SmartScreen may warn → **More info** → **Run anyway**.
> - **Android:** the APK is **debug-signed** so it installs via sideload but is
>   not suitable for Play Store.
>
> See [RELEASE.md](RELEASE.md) for the signing roadmap and release process.

---

## Quick start

1. **Launch the sender** on your laptop. It shows a **6-digit pairing code** and
   a QR code (`ip:port:token`).
2. **Open the receiver** on the TV. The laptop appears automatically; select it
   and enter the 6-digit code. If discovery is blocked, type the full
   `ip:port:token` instead.
3. Pick the display mode and audio route, then click **Start sharing**. The TV
   goes fullscreen and shows your screen.

**Requirements**

- Laptop and TV on the **same local network** (Wi-Fi and/or Ethernet).
- **macOS:** grant **Screen Recording** permission (System Settings → Privacy &
  Security → Screen Recording) and allow incoming connections when prompted.
- **Windows:** allow the app through **Windows Firewall** (Private network) when
  prompted.
- **TV:** Android TV / Google TV, API 21+ (Android 9+ recommended for reliable
  hardware WebRTC decode).

**Network notes (real-world LANs)**

- mDNS works across Wi-Fi bands and Ethernet on the same router/subnet.
- mDNS generally does **not** cross VLANs/subnets (e.g. a separate guest Wi-Fi) —
  use the manual `ip:port:token` fallback.
- Routers with **AP/client isolation** (common on public/office Wi-Fi) block
  device-to-device traffic entirely; the app cannot work around that.

---

## Build from source

**Prerequisites:** Rust (stable), Node.js 22+, JDK 17, Android SDK (for the
receiver). The Rust core links libopus at build time, so a C toolchain +
**cmake** are required (no runtime dependency is added).

```sh
# Rust core — build + unit tests (capture/encode/audio/signaling)
cargo test -p wdt-core

# Sender app (Tauri) — dev run
cd sender-app
npm install
npm run tauri dev          # production build: npm run tauri build

# Receiver app (Android TV) — build APK + unit tests + lint
cd receiver-app
./gradlew assembleDebug    # output: app/build/outputs/apk/debug/app-debug.apk
./gradlew testDebugUnitTest lint
```

Checks that CI also runs:

```sh
cargo fmt --all -- --check          # formatting
npm run build && npm run i18n:check # sender frontend + i18n dictionary guard
python3 receiver-app/scripts/check_i18n.py  # receiver ID/EN parity guard
```

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for the full development journal,
per-task test recipes, and device test checklists.

---

## Status & roadmap

Honest, evidence-based status (details and measurements in
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)):

| Area | Status |
|---|---|
| macOS sender (capture → encode → E2E to TV) | ✅ Runtime-validated on real hardware |
| Windows sender | ⚠️ Implemented + compile-verified; **not** runtime-tested on real hardware |
| Android TV receiver | ✅ Video + audio verified on a low-end TV (~1.8 GB RAM); UI verified |
| End-to-end latency | ~300 ms perceived; ~250 ms is the TV render→panel path (out of our control). Low-latency mode trims the upstream part. |
| Audio routing (4 modes) | ✅ Validated, including live switching without dropping video |
| English / Indonesian UI | ✅ Implemented, CI-guarded |
| Release packaging (dmg/exe/apk) | ✅ Automated via GitHub Actions; **unsigned** |
| Extended display (second monitor) | 🚧 Experimental (macOS virtual display) |
| TalkBack, glass-to-glass lip-sync, Windows runtime | ⏳ Not yet verified (need hardware/camera) |

Roadmap and architecture live in [docs/PLAN.md](docs/PLAN.md); the UX blueprint
(Mirror/Extended, audio routing, i18n, releases) is in
[docs/UI_UX_REVAMP_PLAN.md](docs/UI_UX_REVAMP_PLAN.md).

---

## Repository layout

```text
core/          Rust core: capture, encode, audio, signaling (wdt-core)
sender-app/    Tauri desktop app (macOS/Windows) — Rust backend + TS web UI
receiver-app/  Android TV app (Kotlin + libwebrtc)
docs/          Architecture, protocols, UX plan, development journal
.github/       CI + release workflows
```

## Documentation

| Document | Contents |
|---|---|
| [docs/PLAN.md](docs/PLAN.md) | Goals, tech stack, architecture, roadmap |
| [docs/UI_UX_REVAMP_PLAN.md](docs/UI_UX_REVAMP_PLAN.md) | UX blueprint and feature ordering |
| [docs/SIGNALING_PROTOCOL.md](docs/SIGNALING_PROTOCOL.md) | WebSocket signaling message contract |
| [docs/R5_DECISIONS.md](docs/R5_DECISIONS.md) | Design decisions and trade-offs |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Development journal, test recipes, device checklists |
| [RELEASE.md](RELEASE.md) | Release process, signing roadmap, manual verification |
| [CHANGELOG.md](CHANGELOG.md) | Notable changes per release |

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). In short:
fork, create a topic branch, keep commits in Conventional Commit style
(`feat:`, `fix:`, `docs:`, `ci:`), run the checks above, and open a PR.
`main` is protected: every PR needs green CI and maintainer review, and only
the maintainer may push to `main` directly.

Adding a UI string? Add it to the sender dictionary
(`sender-app/src/i18n.id-en.json`) and/or the receiver resources
(`receiver-app/app/src/main/res/values/` + `values-en/`); CI guards
(`npm run i18n:check`, `scripts/check_i18n.py`) enforce parity.

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). To report a
security issue, see [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE) © 2026 Pandu Norsya'bani.

## Acknowledgements

Built on the shoulders of open source: [libwebrtc](https://webrtc.org/) (via
[`webrtc-rs`](https://github.com/webrtc-rs/webrtc) on the sender and
[`io.github.webrtc-sdk:android`](https://github.com/webrtc-sdk/webrtc) on the
receiver), [Tauri](https://tauri.app/), [`axum`](https://github.com/tokio-rs/axum),
[Opus](https://opus-codec.org/), and Google's public STUN servers for ICE
candidate gathering.
