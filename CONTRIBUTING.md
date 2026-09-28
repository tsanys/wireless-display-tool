# Contributing

Thanks for your interest in Wireless Display Tool (WDT)! This project mirrors a
macOS/Windows screen to Android TV over the local network via WebRTC.

## Scope

Please keep the following project constraints in mind:

- **LAN-only by design.** No cloud, no relay/TURN, no external server in the MVP.
- **Hardware codecs only.** H.264 hardware encode (sender) and decode (receiver);
  no software-encode fallback.
- **Android TV / Google TV** is the receiver target (not Tizen/webOS).
- **One local network.** Sender and receiver must share a broadcast domain.

Ideas that conflict with these constraints are still welcome as issues for
discussion, but may be declined for the MVP.

## Project layout

```text
core/          Rust crate (wdt-core): capture, encode, audio, signaling
sender-app/    Tauri desktop app (Rust backend + TypeScript web UI)
receiver-app/  Android TV app (Kotlin + libwebrtc)
docs/          Architecture, protocols, UX plan, development journal
```

## Development setup

Prerequisites: Rust (stable), Node.js 22+, JDK 17, and the Android SDK for the
receiver. The Rust core links libopus at build time, so a C toolchain and
**cmake** are required.

```sh
# Rust core
cargo test -p wdt-core

# Sender app (Tauri)
cd sender-app
npm install
npm run tauri dev

# Receiver app (Android TV)
cd receiver-app
./gradlew assembleDebug
```

## Checks to run before opening a PR

Please run the checks that CI runs. At minimum:

```sh
cargo fmt --all -- --check        # Rust formatting
cargo test --workspace            # Rust tests (macOS: full workspace)
```

For frontend-only changes:

```sh
cd sender-app
npm run build          # tsc + vite build (typecheck)
npm run i18n:check     # i18n dictionary guard
```

For receiver-only changes:

```sh
cd receiver-app
./gradlew testDebugUnitTest lint assembleDebug
python3 scripts/check_i18n.py       # ID/EN string parity guard
```

> CI note: the macOS workspace job runs on pull requests and manual dispatch
> only (private-repo CI minutes); the Windows and Android jobs run on every push.

## Commit and PR conventions

- Use **Conventional Commits**: `feat:`, `fix:`, `docs:`, `ci:`, `refactor:`,
  `test:`, `chore:`. Scope is optional but encouraged, e.g.
  `feat(R7): resolution/fps/quality controls`.
- Keep commits focused and messages in the imperative mood.
- Reference the milestone tag (e.g. `R7`) when it helps reviewers.
- In a PR, describe **what changed, why, and how you verified it** (commands and
  observed results). Include device/OS details for anything hardware-related.
- Update `CHANGELOG.md` under `[Unreleased]` for user-visible changes.

## Adding or changing UI strings (i18n)

The UI supports Indonesian (default) and English.

- **Sender:** add the entry to `sender-app/src/i18n.id-en.json` (key = Indonesian
  string, value = English) and use `t()` / `tf()` in `sender-app/src/main.ts`.
- **Receiver:** add the string to
  `receiver-app/app/src/main/res/values/strings.xml` (Indonesian) **and**
  `values-en/strings.xml` (English).

CI guards enforce that dictionaries have no empty/untranslated entries and that
receiver strings have ID/EN parity with matching format arguments.

## Reporting bugs and requesting features

Use the issue templates. For bugs, include: platform/OS version, TV model and
Android version, exact steps, expected vs actual behavior, and any sender
console/logcat excerpts. Never paste pairing tokens or private IPs.

## Code of conduct

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

By contributing you agree that your contributions are licensed under the
[MIT License](LICENSE).
