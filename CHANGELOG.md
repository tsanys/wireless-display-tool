# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-09-28

First stable release of Wireless Display Tool: mirror a macOS/Windows screen to
an Android TV over the LAN via WebRTC.

Same feature set as `0.1.0-rc1`, plus:

### Added

- Documentation: English open-source README, project `LICENSE` (MIT), and
  community files (`CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`,
  issue/PR templates).

## [0.1.0-rc1] - 2026-09-28

First pre-release of Wireless Display Tool: mirror a macOS/Windows screen to an
Android TV over the LAN via WebRTC.

### Added

- **Rust core (`wdt-core`)** — screen capture (`ScreenCaptureKit` on macOS,
  `Windows.Graphics.Capture` on Windows), hardware H.264 encoding (VideoToolbox /
  Media Foundation), system audio capture (SCK audio / WASAPI loopback) encoded
  to Opus, mDNS advertising, and an embedded WebSocket signaling server
  (`axum` + `webrtc-rs`).
- **Sender app (Tauri, macOS/Windows)** — guided flow (**Target TV → Display →
  Audio → Start sharing**), QR pairing, help/diagnostics dialogs, capability-gated
  Extended and TV-audio, and per-TV persisted settings.
- **Receiver app (Android TV, Kotlin + libwebrtc)** — NSD discovery with manual
  `ip:port:token` fallback, fullscreen video render, Opus audio playback with
  audio focus, and device capability tier detection.
- **Video controls** — resolution (1080p/720p), frame rate (30/60 fps), and
  quality presets (Balanced / Sharp-text); letterbox scaling and a
  constant-quality encoder mode for sharper text.
- **Audio routing** — four user-facing routes: Laptop / TV / Both / Muted, with
  live switching that does not interrupt video.
- **Low-latency mode** (receiver, experimental) — trims the WebRTC video jitter
  buffer via a field trial; default off, applies on next app start.
- **Localization** — English/Indonesian UI on sender and receiver, following the
  system language, with CI parity guards.
- **Diagnostics export** — local JSON bundle with technical state only (no
  tokens, IPs, or credentials).
- **CI/CD** — GitHub Actions for formatting/tests on macOS (PR only), Windows
  core tests, and Android test/lint/assemble; a Release workflow producing a
  macOS universal `.dmg`, a Windows NSIS `.exe`, and a debug-signed Android
  `.apk`.

### Known limitations

- Windows sender is implemented and compile-verified but **not runtime-tested on
  real hardware**.
- End-to-end perceived latency is ~300 ms, dominated by the TV's render→panel
  path.
- Release artifacts are **unsigned/adhoc** (Gatekeeper/SmartScreen warnings;
  Android APK is debug-signed).
- Extended display remains experimental (macOS virtual display).
- TalkBack and glass-to-glass lip-sync are not yet verified.

[Unreleased]: https://github.com/tsanys/wireless-display-tool/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tsanys/wireless-display-tool/releases/tag/v0.1.0
[0.1.0-rc1]: https://github.com/tsanys/wireless-display-tool/releases/tag/v0.1.0-rc1
