# Wireless Display Tool — Mirror & Extend to Smart TV

**Target device**: semua Android TV / Google TV (bukan cuma Xiaomi) — device Xiaomi TV yang ada sekarang (Android TV 11, quad-core Cortex-A55, ~2GB RAM, Mali GPU) dipakai sebagai **baseline low-end** untuk testing performa
**Laptop source**: macOS & Windows
**Jaringan**: harus jalan di LAN manapun — WiFi (2.4GHz/5GHz), Ethernet, campuran keduanya — selama sender & receiver satu jaringan lokal yang sama
**Status**: Mirror MVP terimplementasi; verifikasi TV, quality tuning, dan
revamp UX/fitur lanjutan masih berjalan

---

## 1. Tujuan & Scope

Membangun tool custom untuk:
1. **Mirror** — screen laptop (Mac/Windows) di-stream real-time ke Smart TV lewat jaringan lokal (WiFi/Ethernet/campuran).
2. **Extend** (fase 2) — TV jadi virtual second monitor, bukan sekadar duplikat layar.
3. **Ekstensi lain** (fase 3, opsional) — remote input (mouse/keyboard dari TV ke laptop), audio routing, multi-client.

Blueprint UX dan kontrak fitur terperinci ada di
[`UI_UX_REVAMP_PLAN.md`](UI_UX_REVAMP_PLAN.md). Dokumen tersebut menjadi acuan
untuk redesign sender/receiver dan urutan implementasi Mirror/Extended/audio.

**Kompatibilitas device**:
- Receiver app ditulis generik untuk **Android TV / Google TV standar** (API level 21+ untuk jangkauan luas, tapi target utama API 28+ / Android TV 9+ karena WebRTC hardware decode lebih reliable di situ), bukan hardcoded untuk satu merek/model.
- Xiaomi TV yang disebut di awal jadi **device uji baseline** karena speknya termasuk yang paling terbatas (RAM ~2GB) — kalau app jalan lancar di situ, kemungkinan besar jalan lebih baik lagi di TV lain (Sony/TCL/Chromecast with Google TV/dll) yang speknya biasanya lebih tinggi.
- App melakukan **capability detection** saat start (cek RAM tersedia & daftar codec hardware yang didukung via `MediaCodecList`) untuk otomatis pilih resolusi/bitrate yang sesuai per device, bukan hardcoded satu preset untuk semua.

**Non-goals (untuk sekarang)**:
- Tidak menargetkan Tizen (Samsung) atau webOS (LG) — fokus Android TV/Google TV dulu (mayoritas merek non-Samsung/LG pakai ini, termasuk Sony, TCL, Xiaomi, Chromecast with Google TV).
- Tidak butuh akses dari luar LAN/internet di MVP — asumsi sender & receiver selalu di jaringan lokal yang sama.
- Tidak publish ke Play Store / App Store di awal — personal use / sideload.

---

## 2. Tech Stack (Final)

| Layer | Pilihan | Catatan |
|---|---|---|
| Core capture/encode/stream | **Rust** | Cross-platform via FFI ke native capture API tiap OS |
| Sender app shell (Win + Mac) | **Tauri** (Rust backend + web frontend) | 1 codebase UI, tetap native performance di core |
| Signaling server | **Rust** (`axum` + `webrtc-rs`) | Di-embed ke dalam sender app, tidak perlu service terpisah |
| Receiver (Android TV) | **Kotlin** + WebRTC Android SDK resmi | Native, ringan — RAM device cuma ~2GB |
| Discovery | **mDNS** (`mdns-sd` di Rust, Android NSD API di receiver) + **fallback manual** (QR code / input IP:port) | Auto-discovery di LAN, dengan fallback kalau mDNS diblokir jaringan |
| Transport | **WebRTC** (P2P, LAN) | Low-latency, adaptive bitrate built-in |
| TURN/STUN | Skip di MVP; `coturn` kalau nanti butuh akses luar LAN | — |

### Capture API per OS (dipanggil dari Rust core)
- **Windows**: `Windows.Graphics.Capture` via crate `windows-rs`
- **macOS**: `ScreenCaptureKit` — akses via `objc2`/`core-graphics` crate dari Rust, atau modul Swift kecil yang di-FFI ke Rust core kalau ScreenCaptureKit binding Rust kurang stabil

### Encoding
- Hardware-accelerated wajib (jangan software encode):
  - Windows: Media Foundation / NVENC (kalau ada GPU discrete)
  - macOS: VideoToolbox
  - Codec: H.264 (baseline/main profile) — device TV target sudah pasti support hardware decode H.264

---

## 3. Arsitektur

```
┌─────────────────────────┐         mDNS discovery        ┌──────────────────────┐
│   Sender App (Tauri)     │ ◄────────────────────────────► │  Receiver App (TV)   │
│   Windows / macOS         │                                 │  Kotlin + WebRTC SDK │
│                           │                                 │                      │
│  ┌─────────────────────┐ │      WebRTC signaling (WS)     │  ┌────────────────┐  │
│  │ Rust Core            │ │ ◄──────────────────────────────┼─►│ Signaling Client│  │
│  │ - Screen capture     │ │      (embedded server)          │  └────────────────┘  │
│  │ - HW encode (H.264)  │ │                                 │  ┌────────────────┐  │
│  │ - Signaling server   │ │      WebRTC media (P2P/SRTP)    │  │ WebRTC decode  │  │
│  │   (axum + webrtc-rs) │ │ ◄──────────────────────────────┼─►│ + render       │  │
│  └─────────────────────┘ │                                 │  └────────────────┘  │
└─────────────────────────┘                                 └──────────────────────┘
```

**Alur koneksi:**
1. Sender app dibuka → embedded signaling server start di port lokal → advertise via mDNS (`_wdt._tcp.local` misalnya).
2. Receiver app di TV discover service via NSD → connect ke signaling server (WebSocket) → tukar SDP offer/answer + ICE candidates.
3. WebRTC PeerConnection terbentuk P2P langsung antara laptop & TV (masih dalam LAN, STUN publik cukup buat ICE gathering meskipun P2P-nya lokal).
4. Rust core mulai capture layar → encode H.264 → kirim lewat WebRTC media track.
5. Receiver decode (hardware-accelerated via `MediaCodec`) → render ke Surface TV.

---

## 4. Jaringan — LAN, WiFi, Ethernet, dan Kondisi Dunia Nyata

Tool ini harus tetap jalan selama sender & receiver berada di **jaringan lokal (broadcast domain) yang sama**, apapun medianya:

| Skenario | Status | Catatan |
|---|---|---|
| Laptop WiFi + TV WiFi (SSID sama) | ✅ Supported | Kasus paling umum |
| Laptop Ethernet + TV WiFi (satu router/switch sama) | ✅ Supported | Selama satu subnet, mDNS tetap jalan lintas WiFi↔Ethernet di router yang sama |
| Laptop WiFi 5GHz + TV WiFi 2.4GHz | ✅ Supported | Beda band tidak masalah selama satu router/SSID group |
| Laptop & TV beda VLAN/subnet (mis. WiFi tamu terpisah dari WiFi utama) | ⚠️ Perlu fallback manual | mDNS multicast umumnya **tidak** cross-subnet/VLAN — jelaskan di UI, arahkan user ke fallback manual IP/QR |
| Router dengan **AP/Client Isolation** aktif (umum di WiFi kantor/publik) | ⚠️ Tidak akan jalan | Device-to-device traffic diblokir di level router — di luar kendali app, tampilkan pesan error yang jelas |

**Implikasi desain:**
1. **Jangan hardcode asumsi WiFi-only** — semua logic capture/stream/signaling network-agnostic, cukup bind ke semua interface lokal (0.0.0.0) di signaling server, bukan interface WiFi spesifik.
2. **mDNS bisa gagal** di beberapa jaringan (router konsumer murah, WiFi kantor/publik dengan isolasi, atau OS Android tertentu yang restrict multicast di background) — **wajib ada fallback**:
   - Manual: user input IP:port sender secara langsung di receiver app.
   - QR code: sender app tampilkan QR berisi `ip:port` + token pairing, discan pakai kamera kalau TV/remote punya, atau ditampilkan sebagai teks untuk diketik manual.
3. **Firewall OS di laptop** (Windows Defender Firewall / macOS firewall) bisa blok incoming connection ke signaling server — sender app perlu request/prompt exception di first run, atau minimal kasih instruksi jelas ke user kalau connection gagal karena ini.
4. **IPv4 sebagai baseline** — jangan asumsikan IPv6 tersedia/routable di semua jaringan rumah, pastikan binding & discovery jalan di IPv4 dulu; IPv6 boleh jadi bonus kalau available, bukan requirement.

---

## 5. Infrastruktur

**MVP (LAN-only)**: **tidak butuh cloud/server terpisah sama sekali.**
- Signaling server jalan sebagai bagian dari sender app di laptop.
- STUN pakai server publik gratis (misal Google STUN) hanya untuk ICE candidate gathering — tetap P2P, bukan relay.

**Fase lanjut (opsional, kalau nanti butuh akses luar LAN)**:
- Deploy signaling server standalone ke VPS kecil (Fly.io/DigitalOcean).
- Tambah `coturn` (TURN server) untuk kasus NAT/firewall strict.
- Tidak masuk scope MVP — dicatat di sini sebagai catatan masa depan saja.

---

## 6. Distribusi & Instalasi

| Komponen | Cara distribusi (fase dev) | Cara distribusi (matang) |
|---|---|---|
| Sender (Windows) | Build lokal `.exe`/`.msi` via `tauri build`, install manual | Sama, tambah auto-updater Tauri |
| Sender (macOS) | Build lokal `.app`/`.dmg`, override Gatekeeper manual | Code signing + notarization (Apple Developer Program, $99/thn) |
| Receiver (Android TV) | Sideload APK via `adb install` | Play Store internal testing track (opsional) |

---

## 7. Roadmap & Fase

### Fase 0 — Setup & Riset Teknis (foundational)
- Validasi capture API pilihan bisa jalan minimal (proof of concept capture 1 frame) di kedua OS.
- Validasi `webrtc-rs` bisa established P2P connection sederhana (data channel dulu, belum video).
- Setup repo monorepo, struktur folder.

### Fase 1 — Mirror MVP (goal utama)
- Sender: capture layar → encode H.264 → kirim via WebRTC video track.
- Signaling server embedded + mDNS advertise.
- Receiver: discover, connect, decode, render full-screen di TV.
- Target: mirror stabil, latency <300ms di LAN, tanpa fitur tambahan (belum ada UI cantik, belum ada remote control).

### Fase 2 — Extend Mode
- Riset & implement virtual display driver:
  - Windows: pakai **IddCx** (Indirect Display Driver framework resmi Microsoft).
  - macOS: riset `CGVirtualDisplay` API (perlu validasi ketersediaan & batasan di versi macOS target).
- Sender bisa pilih source: mirror (capture display utama) vs extend (capture virtual display baru).

### Fase 3 — Ekstensi Fitur (opsional, prioritas belakangan)
- Remote input: TV remote/app kirim event mouse/keyboard balik ke laptop (`SendInput` Windows / `CGEvent` macOS).
- Audio routing bareng video track.
- Multi-client / reconnect handling.

### Fase UX — Revamp Sender & Receiver (bisa paralel setelah Mirror stabil)
- Ubah sender menjadi alur: pilih TV → mode tampilan → lokasi suara → mulai.
- Ubah receiver menjadi UI TV D-pad-first, pairing sederhana, dan overlay
  playback yang auto-hide.
- Capability-gate untuk Extended dan audio: opsi hanya aktif bila backend siap.
- Tahapan dan acceptance criteria: [`UI_UX_REVAMP_PLAN.md`](UI_UX_REVAMP_PLAN.md).

---

## 8. Struktur Repo (usulan)

```
wireless-display-tool/
├── core/                    # Rust core: capture, encode, signaling
│   ├── capture/
│   │   ├── windows.rs       # Windows.Graphics.Capture bindings
│   │   └── macos.rs         # ScreenCaptureKit bindings
│   ├── encode/
│   ├── signaling/           # axum server + webrtc-rs
│   └── Cargo.toml
├── sender-app/               # Tauri shell (UI, tray, settings)
│   ├── src-tauri/            # Rust side, calls into core/
│   └── src/                  # Web frontend (settings UI)
├── receiver-app/             # Android TV app
│   ├── app/src/main/kotlin/
│   └── build.gradle.kts
├── docs/
│   └── PLAN.md               # dokumen ini
└── README.md
```

---

## 9. Task Breakdown untuk Eksekusi AI Agent

> Setiap task idealnya jadi 1 PR/commit terpisah. Urutan mengikuti fase di atas.

### T0 — Setup Repo
- [x] Init monorepo dengan struktur di atas.
- [x] Setup `Cargo.toml` workspace untuk `core/` dan `sender-app/src-tauri/`.
- [x] Setup Android project kosong untuk `receiver-app/` dengan target Android TV (API 30 min).
- [x] Setup CI dasar (build check 3 target) — `.github/workflows/ci.yml`: Windows (`cargo test -p wdt-core`) + Android (gradle test/lint/assemble) tiap push; macOS (fmt + `cargo test --workspace` + npm build) untuk PR/manual. Ketiganya **hijau** di CI (run dispatch nyata).

### T1 — Rust Core: Screen Capture PoC
- [x] Implement capture 1 frame dari display utama di Windows via `windows-rs` (`Windows.Graphics.Capture`), simpan sebagai file gambar untuk verifikasi.
- [x] Implement capture 1 frame di macOS via `ScreenCaptureKit`.
- [x] Unifikasi lewat trait `ScreenCapturer` yang punya implementasi per-platform (conditional compilation `#[cfg(target_os = ...)]`).

### T2 — Rust Core: Hardware Encoding
- [x] Integrasi hardware encoder (VideoToolbox di Mac, Media Foundation di Windows) untuk encode captured frame ke H.264.
- [x] Verifikasi output H.264 valid (decode balik pakai ffmpeg CLI untuk sanity check, tools eksternal saja, bukan dependency runtime).

### T3 — Signaling Server
- [x] Setup `axum` WebSocket server untuk signaling (SDP offer/answer, ICE candidate exchange), bind ke `0.0.0.0` (semua interface, bukan cuma WiFi).
- [x] Integrasi `webrtc-rs` untuk buat PeerConnection dari sisi sender.
- [x] Implement mDNS advertising (`mdns-sd`) saat server start.
- [x] Implement fallback pairing: generate token + tampilkan sebagai data untuk QR code (`ip:port:token`) yang bisa dipakai kalau mDNS gagal.

### T4 — Sender App Shell (Tauri)
- [x] Setup Tauri project, hubungkan ke `core/` sebagai Rust dependency.
- [x] UI minimal: tombol start/stop mirroring, daftar TV yang terdeteksi (dari mDNS/manual scan), tampilkan QR pairing sebagai fallback.
- [x] Wire up: start capture → encode → kirim ke WebRTC track saat user klik "Start".
- [x] Handle firewall exception request (Windows) di first run / kasih instruksi manual kalau signaling server tidak bisa diakses dari luar.

### T5 — Receiver App (Android TV, generik — bukan spesifik Xiaomi)
- [x] Setup project Kotlin dengan WebRTC Android SDK dependency, `minSdk` 21, `targetSdk` sesuai Android TV terbaru.
- [x] Implement NSD discovery untuk nemuin sender di LAN.
- [x] Implement fallback manual: input `ip:port` langsung atau scan/entry dari QR pairing.
- [x] Implement WebSocket signaling client (connect ke signaling server sender).
- [x] Setup `PeerConnection` sisi receiver, terima video track, render ke `SurfaceViewRenderer`.
- [x] Implement capability detection saat start: cek total RAM (`ActivityManager.MemoryInfo`) & codec hardware yang tersedia (`MediaCodecList`), pilih preset resolusi/bitrate sesuai (low/mid/high) — bukan satu preset fixed untuk semua device.
- [x] UI minimal: full-screen video render, indikator status koneksi.
- [x] Test di minimal 1 device low-end (Xiaomi TV yang ada) untuk validasi baseline performa sebelum expand ke device lain.

### T6 — Integrasi End-to-End (Mirror MVP)
- [x] Test end-to-end: sender start → TV discover → connect → video muncul di TV.
- [x] Ukur latency & stabilitas (target <300ms, tanpa drop frame signifikan di LAN normal). → **~300 ms terukur** (upstream ~50 ms; jalur panel ~250 ms), diterima sementara (lihat README T6).
- [x] Handle reconnect dasar (TV/laptop disconnect sementara lalu reconnect).
- [ ] Test kombinasi jaringan E2–E4 (Ethernet↔WiFi, beda band) — **belum diuji** (baseline WiFi↔WiFi ✅, lihat README checklist E).
- [x] Test fallback manual/QR pairing di kondisi mDNS sengaja dimatikan/diblokir (simulasi jaringan yang isolasi multicast). → F1 (app-level, tanpa NSD) **✅ terverifikasi**; F2 (client isolation sejati) tidak dapat diuji karena memblokir unicast juga.
- [ ] Validasi receiver di TV merek lain — **belum** (tidak ada perangkat lain).
- [x] Ketajaman: letterbox 1:1 di sender (Q1) + encoder constant-quality (Q2) — host-verified; **konfirmasi visual di TV ⏳** (lihat §10).

### T7+ — Extend Mode (setelah MVP solid)
- [x] Riset kelayakan `IddCx` untuk Windows virtual display (keputusan & strategi di `docs/R5_DECISIONS.md`).
- [ ] PoC driver IddCx minimal + signing — **belum** (butuh host Windows/WDK).
- [x] Riset kelayakan `CGVirtualDisplay` untuk macOS, cek batasan versi OS.
- [x] Integrasi virtual display sebagai source capture alternatif di sender. → **macOS selesai** (`core/src/vdisplay`, eksperimental, E2E device); Windows pending.

---

## 10. Acceptance Criteria — Mirror MVP (Fase 1)

Legenda status (T6): ✅ terverifikasi · ⏳ menunggu verifikasi manual user ·
➖ belum diuji (tidak ada hardware akses). Kriteria hanya ditandai ✅ bila
sudah dikonfirmasi lewat pengujian nyata (device/jaringan fisik).

- **Sender app di Windows dan macOS tanpa crash** — ➖ macOS: ✅ (app + pipeline
  berjalan, T4/T6 host-verified). Windows: ➖ belum diuji (tidak ada mesin
  Windows) — code path sudah type-check untuk target `x86_64-pc-windows-msvc`.
- **TV muncul di daftar sender via mDNS tanpa input IP manual** — ✅ dikonfirmasi
  user (T4: TV muncul di daftar UI sender; T5: receiver menemukan sender via NSD).
  Catatan: token pairing tetap diketik manual sekali (token sengaja tidak
  di-advertise lewat mDNS demi keamanan — lihat §4).
- **Fallback manual IP/QR saat mDNS gagal** — ⏳ menunggu (checklist F README).
- **Klik "Start" → layar laptop muncul di TV dalam <5 detik** — ⏳ menunggu
  (checklist A; pipeline capture→encode→track sudah host-verified, render di
  TV belum).
- **Latency end-to-end <300ms, tanpa judder berat (baseline Xiaomi TV ~2GB)** —
  ⏳ **~300 ms terukur (sedikit di atas target), diterima sementara.** Dekomposisi:
  upstream (capture→decode) ≈50 ms ✅; jalur render→display/panel ≈250 ms.
  Capture device tidak valid untuk layer video (HWC overlay → artifact ~800 ms).
  Perlu kamera HP / paksa CLIENT untuk mengukur & memperbaiki sisa ~250 ms.
  Lihat README "Investigasi latency (T6)".
- **Koneksi bertahan ≥30 menit, diuji WiFi↔WiFi dan Ethernet↔WiFi** — ⏳ menunggu
  (checklist D & E).
- **Tidak ada kode yang hardcode merek/model TV** — ✅ (review kode: receiver
  generik, capability detection berbasis RAM+codec, tanpa cek merek).

### Catatan teknis T6 (untuk ditinjau saat validasi)
- FPS pipeline: **~17–18 → ~29 fps** setelah macOS pindah dari
  `SCScreenshotManager` one-shot ke **`SCStream` persisten** (bottleneck
  capture terukur ~45 ms → terikat rate stream). `skipped` turun drastis.
- Ketajaman (Q1+Q2, host-verified):
  - **Q1 letterbox di sender** — frame native (mis. 1800x1169) → kanvas
    **1920x1080** proporsional + bar hitam (`scale_letterbox_bgra`), jadi TV
    menampilkan 1:1 tanpa penskalaan perangkat (sebelumnya TV yang scale).
  - **Q2 constant quality** — `EncoderConfig.quality` default `Some(0.9)`;
    VT `Quality` (macOS) / `CODECAPI_AVEncCommonQuality` +
    `RateControlMode=Quality` (Windows), best-effort + fallback bitrate.
    Bukti host (1080p konten bergerak): IDR pertama CBR 10 Mbps 110 kB →
    quality 0.8 215 kB → **quality 0.9 391 kB** (~1,8×); rata-rata 0.9
    ~7,4 Mbps (di bawah budget 10 Mbps).
  - **Q3 cap bitrate quality mode** — `bitrate_bps` menjadi batas 10 Mbps.
    macOS memakai hard cap VT `DataRateLimits` (window 1 detik). Windows
    mengirim `CODECAPI_AVEncCommonMaxBitRate` secara best-effort; dokumentasi
    Microsoft hanya menjamin properti itu pada constrained VBR, jadi efektivitas
    di quality mode masih perlu diverifikasi pada hardware Windows nyata.
    Host benchmark macOS high-detail: quality 0.8 dan 0.9 sama-sama ~7,2 Mbps
    tanpa warning properti (cap 10 Mbps aktif dan menjadi pembatas).
  - **Q4 profile + entropy A/B** — hook pengujian
    `WDT_H264_PROFILE=baseline|main|high` dan `WDT_H264_CABAC=0|1` mengubah
    encoder sekaligus `profile-level-id` SDP. Default produksi tetap
    Baseline+CAVLC sampai Main/High+CABAC terverifikasi pada TV nyata.
    VideoToolbox host menerima ketiganya; quality 0.9 synthetic 1080p:
    Baseline ~7,38 Mbps, Main ~7,20 Mbps, High ~6,89 Mbps pada ~67 fps.
  - E2E host: `pipeline mulai: 1920x1080@30`, ~29 fps, 0 error, kedua sisi
    `TEST LULUS`. Verifikasi **visual** di TV masih ⏳.
- Latency (T6, terukur):
  - Upstream capture→decode (sink) ≈ **50 ms** (probe in-app `LatencyProbeSink`,
    dikoreksi offset jam device↔laptop ≈ −335 ms).
  - Sender capture→kirim ≈ 20–40 ms; encoder **sinkron 1:1**; SDP tanpa
    `playout-delay`; jb ≈34–38 ms; rtt ≈5 ms; loss 0.
  - **Pelajaran penting:** `screencap`/`screenrecord` **tidak reliabel** untuk
    layer video di TV ini — layer video `composition type=DEVICE` (HWC overlay)
    dibaca capture sebagai buffer **basi ~800 ms (konstan, independen fps &
    resolusi)**. Jadi pengukuran latency video via capture adalah artifact;
    gunakan probe in-app atau kamera HP.
  - Menonaktifkan pemrosesan gambar TV (mjc/dnr/mpeg_nr/aisr/mfc/film via
    `settings put global`) tidak menurunkan latency nyata.
  - Sisa ≈250 ms di jalur render→display/panel (batasan/karakteristik display
    TV); status diterima sementara.
- Target fallback sekaligus cap quality mode **10 Mbps**.
- Profile encoder E2E = **H.264 Baseline**, fmtp SDP
  `profile-level-id=42e02a` (level 4.2, sah untuk ~1080p-class).
- Reconnect: **Stop tidak memutus TV** (re-arm); receiver kedua menggantikan
  yang lama (`bye replaced`); token pairing baru dibuat pada setiap launch.

---

## 11. Catatan Risiko

- **RAM TV target cuma ~2GB** — receiver app harus dijaga ringan; test memory usage sejak awal PoC, jangan tunda ke akhir.
- **ScreenCaptureKit & CGVirtualDisplay di macOS** relatif API baru/terbatas — perlu riset kompatibilitas versi macOS sebelum commit ke pendekatan tertentu di Fase 2.
- **IddCx driver signing** — Windows driver perlu signing khusus (EV certificate) untuk load di production; untuk development bisa pakai test-signing mode, tapi perlu diketahui dari awal supaya nggak jadi blocker mendadak di Fase 2.
- **mDNS tidak selalu reliable** — sebagian router konsumer, WiFi kantor/publik dengan client isolation, atau setting Android tertentu bisa blokir multicast discovery. Fallback manual/QR **bukan fitur opsional**, harus ada sejak MVP supaya tool tetap kepake di jaringan yang "sulit".
- **Device Android TV low-end lain** (bukan cuma Xiaomi) mungkin punya batasan hardware decode berbeda — capability detection (T5) penting supaya app tidak asal crash/lag di device yang belum pernah ditest.
