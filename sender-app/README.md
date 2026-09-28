# Sender App (Tauri)

Desktop sender for **Wireless Display Tool** — a Tauri app (Rust backend +
TypeScript web UI) that captures the screen, encodes H.264 in hardware, and
streams it to an Android TV over the LAN via WebRTC. It also embeds the
signaling server.

See the [root README](../README.md) for the project overview, architecture, and
release downloads. Full development notes live in
[docs/DEVELOPMENT.md](../docs/DEVELOPMENT.md).

## Development

```sh
npm install
npm run tauri dev      # run the app
npm run tauri build    # production bundle (.dmg / .exe via Tauri)
```

Frontend-only checks:

```sh
npm run build          # tsc + vite build (typecheck)
npm run i18n:check     # i18n dictionary parity guard
```

Requires the Rust stable toolchain and **cmake** (the core statically links
libopus at build time).

## Recommended IDE setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
