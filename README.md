# Wireless Display Tool

Mirror/extend laptop (Mac/Windows) ke Smart TV Android TV via WebRTC.
Lihat [docs/PLAN.md](docs/PLAN.md) untuk arsitektur dan roadmap lengkap, serta
[docs/UI_UX_REVAMP_PLAN.md](docs/UI_UX_REVAMP_PLAN.md) untuk blueprint redesign
sender/receiver, Mirror/Extended, dan routing audio.

## Development — Testing Capture (T1)

PoC screen capture: 1 frame dari display utama → file PNG.
Kedua example memakai trait generik yang sama (`ScreenCapturer`,
`core/src/capture/mod.rs`) dengan format pixel **BGRA8**.

### macOS

```sh
cargo run -p wdt-core --example capture_test_macos
# output: core/examples/output/macos_capture_test.png
```

Izin Screen Recording (hanya sekali):

1. Saat pertama dijalankan, macOS menampilkan prompt — atau example
   exit dengan pesan `PermissionDenied` yang jelas (tidak crash).
2. Buka **System Settings > Privacy & Security > Screen Recording**,
   aktifkan untuk biner ini, lalu **jalankan ulang** example-nya.
3. Buka PNG hasilnya dan pastikan sesuai isi layar aktual.

### Windows (jalankan di mesin Windows)

```sh
cargo run -p wdt-core --example capture_test_windows
# output: core/examples/output/windows_capture_test.png
```

Tidak ada prompt izin khusus. Pastikan ada display aktif (bukan sesi
headless tanpa display). Kursor ikut tercapture, tanpa border kuning.

### Cek unifikasi trait (semua platform)

```sh
cargo test -p wdt-core
```

Test `default_capturer_is_usable_generically` memastikan kode downstream
(T2/T4) cukup memanggil `default_capturer()` + method trait tanpa
`#[cfg]` platform.

## Development — Testing Encoding (T2)

PoC integrasi capture→encode: 30 frame dari display utama di-encode
H.264 hardware (VideoToolbox di macOS / Media Foundation MFT di Windows)
→ file bitstream mentah. Default: 1080p-class, 5 Mbps CBR, profile Main
tanpa B-frame, IDR tiap 60 frame, output Annex B dengan SPS/PPS
disisipkan sebelum tiap IDR.

### Run test encode

```sh
cargo run -p wdt-core --example capture_encode_test
# output: core/examples/output/encode_test.h264
```

Catatan Windows: mesin tanpa hardware encoder (mis. VM tanpa GPU) akan
exit dengan error `NoHardwareEncoder` — ini disengaja (strict HW-only,
tanpa fallback software).

### Verifikasi dengan ffmpeg (tools eksternal, bukan dependency)

1. **Decode seluruh stream** — harus 30 frame tanpa error:

   ```sh
   ffmpeg -v error -i core/examples/output/encode_test.h264 -f null -
   ```

   Tidak ada output = semua frame valid. (Tambah `-v info` untuk melihat
   jumlah frame & resolusi.)

2. **Convert ke MP4 untuk ditonton manual:**

   ```sh
   ffmpeg -y -i core/examples/output/encode_test.h264 \
     -c:v copy core/examples/output/encode_test.mp4
   open core/examples/output/encode_test.mp4   # macOS; di Windows: start
   ```

   Gambar harus sesuai isi layar saat test dijalankan — bukan
   hijau/noise/korup. `signalstats` untuk cek objektif:

   ```sh
   ffmpeg -i core/examples/output/encode_test.h264 -vf signalstats,metadata=print -f null - 2>&1 | grep YAVG
   ```

   Nilai YAVG yang bervariasi wajar antar frame dan YMIN/YMAX yang
   menjangkau 0/250 menandakan konten nyata.

### Cek unifikasi trait encoder

```sh
cargo test -p wdt-core   # termasuk default_encoder_is_usable_generically
```

## Development — Testing Signaling Server (T3)

Kontrak pesan: `docs/SIGNALING_PROTOCOL.md` (wajib dibaca sebelum
implementasi receiver Kotlin di T5). Server bind `0.0.0.0:8420`,
endpoint `/ws`, token pairing 6-digit per start.

### Test B — mDNS advertising terlihat tool standar

```sh
# Terminal 1: jalankan server demo (catat token + instance name)
cargo run -p wdt-core --example signaling_server_demo 8431

# Terminal 2: browse service
dns-sd -B _wdt._tcp local.
# Harus muncul: Add ... _wdt._tcp.  WDT <hostname>

# Terminal 3: resolve + cek TXT record
dns-sd -L "WDT <hostname>" _wdt._tcp local.
# Harus muncul: reachable at <host>.local.:8431 + "proto=1 ver=0.1.0"
```

### Test A — handshake WebRTC end-to-end via 2 test client

```sh
# Terminal 1: server demo (catat TOKEN)
cargo run -p wdt-core --example signaling_server_demo 8432

# Terminal 2: test client sender (loopback, tanpa token)
cargo run -p wdt-core --example signaling_test_client -- \
  --role sender --url ws://127.0.0.1:8432/ws

# Terminal 3: test client receiver (dengan token)
cargo run -p wdt-core --example signaling_test_client -- \
  --role receiver --url ws://127.0.0.1:8432/ws --token <TOKEN> --device-id test-tv
```

Sukses = kedua client mencetak `TEST LULUS` dan exit 0. Artinya:
hello/token OK, offer/answer ter-relay, trickle ICE dua arah,
PeerConnection Connected, DataChannel `ctrl` tukar "ping"→"pong".

### Unit test (tanpa jaringan eksternal)

```sh
cargo test -p wdt-core signaling::
```

Mencakup: stabilitas wire-format JSON, relay + error server
(badToken/senderBusy/unexpected/bye), dan handshake 2 PC in-process.

## Development — Running Sender App (T4)

Shell Tauri (Windows/macOS) yang meng-embed signaling server T3 +
sesi sender WebRTC. UI memakai alur **TV tujuan → Tampilan → Suara → Mulai
berbagi**, dengan QR pairing, bantuan, dan diagnostik di dialog sekunder.
Extended dan audio-TV ditampilkan sebagai capability-gated sampai backend siap.

### Jalankan dev mode

```sh
cd sender-app
npm install        # sekali saja (termasuk dep `qrcode` untuk QR)
npm run tauri dev
```

Yang terjadi saat app start (otomatis, tanpa klik):

1. Signaling server bind `0.0.0.0:8420` + advertise mDNS `WDT <hostname>`.
2. Sesi sender connect WS loopback sebagai role `sender`.
3. UI menampilkan token + QR `ip:port:token`.

### Requirement per OS

- **macOS**: dialog "allow incoming network connections" → klik Allow
  (sekali). Screen Recording belum dibutuhkan di T4 (capture mulai T6).
- **Windows**: dialog "Windows Security Alert" otomatis muncul saat
  listen pertama → centang Private → Allow access. Kalau koneksi dari TV
  tetap gagal, buka panel "Bantuan Koneksi" di app untuk instruksi manual
  (command `get_firewall_help`).

### Simulasi receiver (sebelum T5 ada)

Pakai test client T3 sebagai TV palsu — ambil token dari UI app,
lalu di terminal lain:

```sh
cargo run -p wdt-core --example signaling_test_client -- \
  --role receiver --url ws://127.0.0.1:8420/ws --token <TOKEN> --device-id test-tv
```

TV harus muncul di daftar UI ("test-tv"); klik Start → status
`offering → connecting → connected` mengikuti state PeerConnection
aktual. Stderr test client menampilkan sisi receiver.

### Preview state UI tanpa Tauri/TV

```sh
cd sender-app
npm run dev
# buka salah satu:
# http://127.0.0.1:1420/?mock=ready
# http://127.0.0.1:1420/?mock=connecting
# http://127.0.0.1:1420/?mock=connected
# http://127.0.0.1:1420/?mock=error
```

Mock hanya untuk visual development. Capability tetap divalidasi ulang oleh
backend pada command `start_sharing`, sehingga UI tidak bisa memaksa fitur yang
belum tersedia.

## Development — Running Receiver App (T5)

App Android TV (Kotlin, `org.webrtc` via `io.github.webrtc-sdk:android`).
Alur: scanning sender (NSD/mDNS) atau input manual → connect →
connecting → connected (video fullscreen). Overlay kiri-atas menampilkan
status, **device tier** (hasil capability detection), dan nama sender.

### Build APK

```sh
cd receiver-app
./gradlew assembleDebug
# output: app/build/outputs/apk/debug/app-debug.apk
```

Unit test kontrak protokol (tanpa device):

```sh
./gradlew testDebugUnitTest   # 12 test: casing JSON, parse pesan, pairing, tier
```

### Install ke Android TV

```sh
adb connect <IP_TV>:5555        # atau USB debugging
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

Syarat device: Android TV / Google TV (minSdk 21, target Android 11+
disarankan); TV dan laptop sender di **satu LAN** (WiFi atau Ethernet).

### Cara pakai

1. Jalankan sender app di laptop (lihat section T4) — catat **token
   6-digit** di UI sender.
2. Buka WDT Receiver di TV. Daftar sender muncul otomatis via mDNS
   (nama instance `WDT <hostname>`).
3. Ketik token 6-digit di field manual (token sengaja **tidak**
   di-advertise lewat mDNS), lalu pilih sender → `Connect`.
   Bila mDNS diblokir, ketik `ip:port:token` (mis.
   `192.168.1.5:8420:123456`) lalu `Connect`.
4. Saat user menekan **Start Mirroring** di sender, status berubah
   `connecting → connected`; video tampil fullscreen.

### Device tier (capability detection)

Deteksi saat start: total RAM (`ActivityManager.MemoryInfo`) + hardware
H.264 decoder (`MediaCodecList.REGULAR_CODECS`, `isHardwareAccelerated()`
di API 29+ atau heuristik nama di API < 29).

| Tier | Syarat |
|---|---|
| `LOW` | RAM <= 2.5 GB **atau** tanpa HW H.264 decoder |
| `MID` | RAM 2.5–4 GB **dan** ada HW H.264 decoder |
| `HIGH` | RAM > 4 GB **dan** ada HW H.264 decoder |

Tier ditampilkan di overlay untuk verifikasi manual (baseline uji:
Xiaomi TV ~2 GB → `LOW`). Pengiriman tier ke sender belum ada di
protokol v1 — direncanakan di T6.

### Catatan hasil verifikasi T5 di device (MiTV-MOOR2, Android 11, RAM ~1.8 GB)

Diverifikasi langsung di TV fisik:
- Discovery NSD menemukan sender; connect signaling + token berhasil
  (`Pairing OK`).
- Negosiasi SDP sukses dan **PeerConnection CONNECTED**
  (`iceConnectionState=CONNECTED`, pair host terpilih).
- Capability detection device: `tier=LOW ram=1787MB hwH264=true [OMX.MS.AVC.Decoder]`.
- Disconnect bersih: saat sender mengirim `bye`, TV kembali ke layar scan
  tanpa crash.
- Video belum berisi frame karena sender masih memakai video track
  placeholder (tanpa encoder) — integrasi capture→encode→track penuh
  adalah T6.

Dua temuan interop penting yang diperbaiki saat verifikasi device:

1. **Offer harus video-only.** Offer dengan m-line `m=application`
   (DataChannel `ctrl`) membuat libwebrtc menolak m-line tersebut
   (`Failed to setup RTCP mux`), sehingga negosiasi gagal. Jalur produksi
   kini memakai `SenderMode::VideoOnly` (offer hanya `m=video`); mode
   `WithCtrl` tetap ada untuk test T3 (ping/pong).
2. **Codec H.264 harus punya fmtp yang cocok dengan libwebrtc.** Tanpa
   `profile-level-id=42e01f;packetization-mode=1`, libwebrtc mencatat
   "No video codecs in common" dan **menolak m=video di answer**
   (`m= section '0' being rejected`) → tak ada transport → tak ada
   kandidat ICE → koneksi tak pernah terbentuk.

### Debug tool (opsional, tanpa device)

```sh
# Lihat SDP offer yang dihasilkan webrtc-rs (cek fmtp/rtcp-fb/bundle)
cargo run -p wdt-core --example print_offer

# Lihat sdpMid/sdpMLineIndex kandidat ICE yang di-relay
cargo run -p wdt-core --example print_candidate
```

### Flag test client sender

Test client `--role sender` default memakai `WithCtrl` (sesuai Test A T3,
ping/pong). Untuk menguji jalur produksi (offer video-only) di TV:

```sh
cargo run -p wdt-core --example signaling_test_client -- \
  --role sender --video-only --url ws://127.0.0.1:8420/ws
```

## Development — Full E2E Testing (T6)

Panduan uji end-to-end sender (laptop) ↔ receiver (Android TV) beserta
checklist manual. Legenda status: ✅ terverifikasi, ⏳ menunggu, ❌ gagal.

### Prasyarat
- Sender app (T4) jalan; receiver APK (T5/T6) terinstall di TV.
- Laptop & TV di **satu LAN** (WiFi atau Ethernet).
- **macOS**: izinkan **Screen Recording** untuk binary sender app
  (`target/debug/sender-app` saat `tauri dev`) — berbeda dari binary CLI
  example. Tanpa izin, UI sender menampilkan error `capturer: …PermissionDenied`.
- **Windows**: izinkan saat prompt Windows Firewall muncul (Private).

### Menjalankan
```sh
cd sender-app && npm run tauri dev     # catat token di UI
# TV: buka WDT Receiver → isi token → pilih "WDT …" → Connect
# laptop: klik Start Mirroring
```
Konsol sender mencetak `[wdt] pipeline mulai: WxH@fps ssrc=… pt=…`.
Overlay TV menampilkan `stats: fps … · kbps … · jb … ms · rtt … ms · lost … · frames …`.

### Checklist A — Video dasar (⏳ menunggu konfirmasi visual)
| # | Langkah | Harapan | Hasil |
|---|---|---|---|
| A1 | Start Mirroring | status UI `offering → connecting → connected` | |
| A2 | TV | layar laptop tampil, warna benar (bukan hijau/noise), mengikuti gerakan | |
| A3 | UI sender | baris `stats:` muncul (fps/frame/skipped/error) | |
| A4 | Muat pertama | TV menampilkan gambar < 5 dtk setelah klik Start | |
| A5 | Stop → Start lagi | video berhenti lalu kembali **tanpa restart app** | |

### Checklist B — Latency (< 300 ms)
Metode (dipilih): stopwatch manual + stats WebRTC.
1. Tampilkan stopwatch di laptop; arahkan kamera/mata ke layar TV, foto
   satu frame menampilkan keduanya → selisih = latency kasar.
2. Alternatif: gerakkan mouse cepat, bandingkan posisi kursor laptop vs TV.
3. Catat dari overlay TV: `rtt` (ms) dan `jb` (jitter buffer, ms); dari UI
   sender: `fps` dan `skipped`.

| Skenario | rtt | jb | latency kasar | catatan |
|---|---|---|---|---|
| WiFi↔WiFi | | | | |

### Checklist C — Reconnect
| # | Langkah | Harapan | Hasil |
|---|---|---|---|
| C1 | Saat streaming, matikan WiFi TV 5–10 dtk lalu nyalakan | sender tidak crash; UI sender kembali `idle`; TV kembali ke layar scan | |
| C2 | Connect lagi dari TV (token sama) lalu Start | streaming pulih **tanpa restart** app di kedua sisi | |
| C3 | Catat waktu pulih | | |

### Checklist D — Stabilitas (≥ 30 menit)
| # | Cek tiap ~5 menit | Harapan | Hasil |
|---|---|---|---|
| D1 | UI sender `fps`/`skipped` | stabil, tidak naik terus | |
| D2 | Overlay TV `lost` | ~0 / tidak tumbuh berarti | |
| D3 | Tidak ada crash / disconnect | | |
| D4 | RAM sender & TV (opsional, `adb shell dumpsys meminfo com.wdt.receiver`) | tidak tumbuh tanpa batas | |

### Checklist E — Kombinasi jaringan
| Skenario | Cara | Yang dicek | Hasil |
|---|---|---|---|
| E1 WiFi↔WiFi (baseline) | keduanya di SSID sama | discovery + streaming | |
| E2 Ethernet↔WiFi | laptop via kabel, TV WiFi | idem | |
| E3 2.4GHz vs 5GHz | TV di 2.4, laptop di 5 (SSID sama) | idem | |
| E4 beda band + Ethernet | kombinasi lain yang tersedia | idem | |

### Checklist F — Fallback manual saat mDNS gagal
| # | Langkah | Harapan | Hasil |
|---|---|---|---|
| F1 | **App-level**: abaikan daftar NSD, ketik `ip:port:token` di field manual lalu Connect | connect + streaming berhasil | |
| F2 | **Network-level** (opsional): aktifkan *client isolation* / taruh TV di VLAN berbeda | NSD gagal → UI tetap bisa connect via input manual (unicast TCP) | |

Catatan F2: client isolation sejati juga memblokir TCP unicast antar-device,
sehingga fallback tidak bisa menolong (sesuai PLAN bagian 4) — gunakan F1
untuk menguji jalur fallback aplikasi.

### Status hasil uji T6 (MiTV-MOOR2, Android 11, ~1.8 GB)

| Aspek | Hasil |
|---|---|
| Video E2E | ✅ tampil live di TV (diverifikasi via screenshot device) |
| Resolusi stream | **compose letterbox 1920x1080** (TV tampil 1:1, tanpa skala) |
| FPS pipeline | ~29 fps (macOS `SCStream` persisten; sebelumnya ~17) |
| Packet loss | 0 (host+device) |
| RTT / jitter buffer | 4–7 ms / ~57–160 ms |
| Fallback manual (tanpa NSD) | ✅ bisa connect |
| Stabilitas singkat | ✅ aman |
| Kualitas visual | ⏳ menunggu konfirmasi visual ulang (letterbox + constant-quality) |
| Latency E2E (dirasakan) | **~300 ms** (upstream ~50 ms + jalur display ~250 ms) — diterima sementara |
| Reconnect | ⏳ menunggu uji ulang (Stop kini TIDAK memutus TV; token dipersist) |

#### Perbaikan ketajaman (T6 Q1+Q2, host-verified)

Masalah: gambar kabur karena (a) TV meng-*scale* stream beresolusi non-16:9
ke panel 1080p, dan (b) mode **CBR** menekan I-frame sehingga detail teks
hilang.

Solusi:
- **Q1 — letterbox di sender** (`scale_letterbox_bgra`): frame native
  (mis. 1800x1169) diskala proporsional lalu di-*center* di kanvas
  **1920x1080** dengan bar hitam (pillarbox 128 px). TV menampilkan 1:1,
  jadi tidak ada penskalaan perangkat.
- **Q2 — encoder constant quality** (`EncoderConfig.quality`, default
  `Some(0.9)`): VT `kVTCompressionPropertyKey_Quality` (macOS) dan
  `CODECAPI_AVEncCommonQuality` + `RateControlMode=Quality` (Windows),
  best-effort dengan fallback ke bitrate bila encoder menolak.
- **Q3 — cap bitrate quality mode**: `bitrate_bps` menjadi batas 10 Mbps.
  macOS memakai hard cap VideoToolbox `DataRateLimits` dengan window 1 detik;
  Windows mengirim `AVEncCommonMaxBitRate` secara best-effort karena kontrak
  Media Foundation hanya menjaminnya untuk constrained VBR. Jalur Windows
  masih perlu verifikasi pada hardware nyata. Benchmark macOS high-detail
  (`WDT_BENCH_SYNTHETIC=1 cargo run -p wdt-core --example bench_quality`)
  menghasilkan ~7,2 Mbps untuk quality 0.8 dan 0.9 tanpa warning properti,
  sehingga cap 10 Mbps aktif dan menjadi pembatas pada skenario tersebut.
- **Q4 — A/B profile + entropy**: encoder dan `profile-level-id` SDP sekarang
  bergerak bersama. Default jalur produksi tetap **Baseline+CAVLC** untuk
  kompatibilitas; Main/High+CABAC diaktifkan khusus pengujian sampai lolos TV:

  ```sh
  WDT_H264_PROFILE=main WDT_H264_CABAC=1 npm run tauri dev
  # atau benchmark encoder tanpa TV:
  WDT_BENCH_SYNTHETIC=1 WDT_H264_PROFILE=high WDT_H264_CABAC=1 \
    cargo run -p wdt-core --example bench_quality
  ```

  Nilai profile: `baseline`, `main`, `high`; CABAC: `0` atau `1`.
  Kombinasi Baseline+CABAC ditolak karena tidak valid menurut H.264.

  Benchmark hardware VideoToolbox, synthetic high-detail 1080p/120 frame:

  | Tuning | quality 0.9 | Kecepatan encode |
  |---|---:|---:|
  | Baseline+CAVLC | ~7,38 Mbps | ~67,7 fps |
  | Main+CABAC | ~7,20 Mbps | ~66,8 fps |
  | High+CABAC | ~6,89 Mbps | ~67,2 fps |

  Angka ini memverifikasi encoder host, bukan kemampuan decoder TV.

Bukti host (`cargo run -p wdt-core --example bench_quality`, 1920x1080,
konten bergerak):

| Mode | IDR pertama | rata-rata bitrate |
|---|---|---|
| CBR 10 Mbps | 109.869 byte | ~4,2 Mbps |
| constant-quality 0.8 | 214.891 byte | ~2,6 Mbps |
| constant-quality 0.9 | **390.598 byte** (~1,8× dari 0.8) | ~7,4 Mbps |

E2E host (`signaling_server_demo` + sender/receiver `--video-only`):
`pipeline mulai: 1920x1080@30`, **~29 fps, 0 error**, sender & receiver
`TEST LULUS`.

Perilaku reconnect (T6):
- **Stop** hanya menghentikan streaming; TV tetap terhubung & siap — tekan
  **Start** lagi tanpa menyentuh TV.
- **Receiver kedua** dengan token valid menggantikan yang lama (yang lama
  diberi `bye replaced` → kembali ke layar scan).
- **Token dipersist** di app-data sender → restart app tidak mengubah token.

#### Investigasi latency (T6) — hasil & pelajaran

Dekomposisi (MiTV-MOOR2, stream 640×360–1920×1080, 30 fps):

| Komponen | Nilai | Cara ukur |
|---|---|---|
| Sender capture→kirim | ~20–40 ms | probe strip di pipeline (in-app) |
| Jaringan (jb / rtt / loss) | ~34–38 ms / ~5 ms / 0 | stats WebRTC |
| Upstream capture→decode (sink) | **~50 ms** | `LatencyProbeSink` in-app (+ kalibrasi offset jam device↔laptop) |
| Jalur render→display/panel | **~250 ms** | selisih total (~300 ms dirasakan user) − upstream |
| **Total E2E dirasakan** | **~300 ms** | penilaian user (lingkaran mouse) |

Temuan penting:
- **`screencap`/`screenrecord` tidak reliabel untuk layer video** di TV ini:
  layer video dikomposisi `composition type=DEVICE` (HWC overlay), sedangkan
  UI app = `CLIENT`. Capture membaca buffer video yang **basi ~800 ms (konstan,
  independen fps & resolusi)** → angka "latency video ~800 ms" dari capture
  adalah **artifact pengukuran**, bukan latency nyata.
- A/B resolusi (640×360 vs 720p vs 1080p) dan fps (5 vs 30) menghasilkan skew
  capture yang sama → **bukan** decode-throughput.
- Menonaktifkan pemrosesan video TV (`mjc_effect`, `dnr`, `mpeg_nr`, `aisr`,
  `mfc_smooth`, `film_mode` via `settings put global`) **tidak** menurunkan
  latency nyata.
- Encoder sender **sinkron 1:1** (tidak ada penundaan); SDP tanpa `playout-delay`.

Sisa ~250 ms ada di jalur render→display/panel TV (tak terukur lewat capture
karena overlay hardware). Untuk mengukur/memperbaikinya perlu **kamera HP**
(ground truth panel) atau memaksa komposisi CLIENT (mis. `debug.sf.hwc.disabled`
+ restart SurfaceFlinger). Status: **diterima sementara**.

Alat ukur yang dipakai (opsional, off di produksi):
- `LatencyProbeSink` (receiver) — gate `ATTACH_LATENCY_PROBE` (default `false`).
- Hook uji sender: `WDT_TARGET` (resolusi), `WDT_FPS` (fps), `WDT_NO_C1`
  (stream kontinu tanpa uji Stop→Start), `WDT_TOKEN` (server demo token tetap).

### Batasan pengukuran
- `rtt`/`jb` adalah metrik WebRTC (bukan glass-to-glass).
- **Capture device (`screencap`/`screenrecord`) tidak valid untuk layer video
  HWC** — jangan pakai untuk mengukur latency video di TV ini.
- FPS pipeline ~29 di host (debug build dgn `[profile.dev.package.wdt-core]
  opt-level = 2`) — lihat catatan T6.

## Development — Audio Routing (R4)

Audio sistem laptop dikirim ke TV sebagai track Opus WebRTC, dengan routing
user-facing: **Laptop / TV / Keduanya / Tanpa suara**.

```text
SystemAudioCapturer (macOS SCK audio / Windows WASAPI loopback)
   └─ PCM f32 stereo → StreamResampler (48 kHz; passthrough di macOS)
        └─ OpusFrameChunker (960 frame = 20 ms) → Opus encoder (48 kbps–96 kbps)
             └─ TrackLocalStaticSample → Android libwebrtc AudioTrack + audio focus
```

### Dependency audio (tradeoff)

- **Opus**: crate `opus` (→ `opusic-sys`) meng-compile libopus via **cmake saat
  build lalu static-link**. Tidak ada DLL/runtime native tersembunyi; aplikasi
  tetap satu biner. cmake adalah kebutuhan **build-time** (seperti rustc/toolchain
  C), bukan runtime. Alternatif `audiopus` ditolak karena menuntut autotools
  (`autoreconf`/`aclocal`) saat build.
- **Resampler**: `rubato` (pure-Rust, tanpa C). macOS meminta 48 kHz langsung
  dari ScreenCaptureKit sehingga jalur macOS passthrough (tanpa delay FFT);
  `rubato` hanya aktif bila mix-format WASAPI ≠ 48 kHz.

### Modul & batas

`core/src/audio/`: `mod.rs` (trait `SystemAudioCapturer`, `AudioFrame`,
capability) · `pcm.rs` (normalisasi + rechunk) · `resample.rs` · `opus_encode.rs` ·
`pipeline.rs` (route watch + backpressure bounded) · `macos.rs` (SCK audio) ·
`windows.rs` (WASAPI loopback).

### Verifikasi host (macOS)

```sh
# Capture audio sistem nyata (butuh izin Screen Recording):
cargo run -p wdt-core --example audio_capture_probe
# contoh keluaran: "hasil: 147 frame, 282240 sampel, peak=0.198 rms=0.0198"
```

Unit test (tanpa device): `cargo test -p wdt-core audio::` — mencakup
normalisasi PCM, resample 44,1→48 kHz, durasi paket Opus 20 ms, timestamp
monotonik, route Laptop/Muted tidak menulis sampel, SDP AV (m=video+m=audio),
PT/SSRC per media kind, dan protocol wire-format (termasuk backward-compat).

### Uji receiver Android

```sh
cd receiver-app && ./gradlew testDebugUnitTest lint assembleDebug
```

### Hasil uji device (MiTV-MOOR2, Android 11, ~1.8 GB)

Skenario (sender = `signaling_test_client --role sender --video-only` +
`WDT_AUDIO=tv|both|muted`, receiver APK terbaru via ADB `10.10.70.24:5555`):

| Skenario | Hasil |
|---|---|
| Route TV | Video 24–29 fps, audio ter-decode/playout, `audioKbps` 28–69 saat audio diputar |
| Route Keduanya | Sama seperti TV (audio lokal tetap hidup) |
| Route Tanpa suara | Audio idle, tidak ada sample ke TV, output OS tidak diubah |
| Live switching Tv→Laptop→Both→Tv | Video **tidak putus** (28 fps stabil), audio pause/resume, 0 error |
| Recovery Stop→Start | Sesi selamat, offer baru, audio re-attach, tanpa pairing ulang |
| Teardown (sender mati / Stop) | `abandonAudioFocus` + `stopPlayout`; tidak ada audio tersisa |

Bukti logcat penting: `audio track ter-attach: wdt-audio enabled=true`,
`WdtAudioFocus: requestAudioFocus=true` / `abandonAudioFocus`,
`WebRtcAudioTrackExternal: startPlayout` / `stopPlayout`,
`sessionConfig: enabled=… route=…`, `peerState=CONNECTED`, `Video tampil ✓`.

### Pengukuran (debug build, 1080p30)

| Metrik | Video-only | Video+audio |
|---|---:|---:|
| CPU sender (1 core, debug) | ~39 % | ~40 % |
| CPU receiver (proses) | ~32 % | ~30,5 % |
| Packet loss audio (local) | — | 0 |
| RSS sender (release) | ~95 MB | ~95 MB |

### Perbaikan memori encoder VideoToolbox (ditemukan saat uji R4)

Soak pipeline (tanpa jaringan) menemukan churn VM/swap: **RSS melonjak beberapa
GB dan ~5 GB halaman tertukar (swapped) dalam <1 menit** saat jalur encode
aktif. Isolasi menunjukkan sumbernya **VideoToolbox**, bukan capture/scale/audio
(`scale` saja: RSS datar 79 MB, 0 swap; `scale+encode`: 5,6 GB, 5,2 GB swap).

Akar masalah: `core/src/encode/macos.rs::wrap_frame` mengalokasikan `Vec`+`Box`
~8 MB **per frame** dan membuat `CVPixelBuffer` baru tiap frame. Karena
`encode_frame` menunggu callback output VT (sinkron), pixel buffer bebas
dipakai ulang → encoder kini membuat **satu** pixel buffer sekali dan
menimpanya tiap frame (`copy_frame_into`).

Hasil setelah perbaikan: **RSS datar ~92 MB, 0 swap, 29 fps**; bitstream tetap
valid (`capture_encode_test` → `ffmpeg` decode tanpa error); E2E device tetap
29 fps + audio ~71 kbps. Reproduksi:

```sh
WDT_SOAK_SECS=90 WDT_SOAK_SYNTH=1 cargo run --release -p wdt-core --example pipeline_soak
# RSS harus datar (bukan tumbuh ke GB). Tanpa WDT_SOAK_SYNTH memakai capture SCK nyata.
```

Encoder **Windows** (`encode/windows.rs`) memiliki pola serupa (alokasi NV12
`Vec` + `MFCreateSample`/`MFCreateMemoryBuffer` per frame). Audit statis
menemukannya dan sudah diperbaiki dengan pola reuse yang sama (buffer NV12 +
sample input persisten, diisi ulang tiap frame). **Validasi:** kompilasi
`cargo check --target x86_64-pc-windows-msvc` bersih (0 warning), tetapi
**belum divalidasi runtime** (tidak ada host Windows). Catatan: reuse sample
input mengandalkan MFT sinkron (data boleh dilepas setelah `ProcessInput`
kembali) — perilaku standar MFT encoder sinkron.

Jalur **audio WASAPI** (`audio/windows.rs`) juga dioptimalkan: `read_frame_into`
(trait `SystemAudioCapturer`, dengan default kompatibel) memakai ulang
kapasitas `AudioFrame.samples`, dan `pcm::convert_to_stereo_f32_into` mengisi
buffer itu langsung dari slice device (tanpa `Vec` perantara). macOS memakai
implementasi default (perilaku tak berubah) — pipeline audio kini menyimpan
satu `AudioFrame` reusable. Efek alokasi per-paket audio memang kecil
(~KB vs MB/frame video), jadi ini kebersihan tambahan, bukan perbaikan churn.

### Hasil uji ter-otomasi (post-fix, 2026-09-27)

| Uji | Metode | Hasil |
|---|---|---|
| Suite lengkap | fmt + 64 test Rust + npm + gradle | **LULUS** |
| Soak pipeline 10 menit | `pipeline_soak` release + sampel RSS | **PASS**: RSS 74–93 MB datar, swap 9,9 MB, 17.537 frame @29,2 fps |
| Stabilitas E2E + live switch | sender cycle route/15 dtk, total ~21 menit lintas instance | **PASS**: 56+ switch, 0 error, 0 crash, focus request/abandon seimbang (22/22), fps-zero maks 4 dtk, audioLost maks 4, RSS sender 114–132 MB |
| Drift audio 10+ menit | `sent_ms == packets × 20` | **0 drift** (31.672 paket = 633.440 ms eksak) |
| Receiver dimatikan saat audio aktif | `adb shell am force-stop` | **PASS**: sender selamat (`ReceiverLeft`, sesi hidup); resume dengan Start baru |
| Sender dimatikan saat audio aktif | `pkill` sender | **PASS**: TV teardown bersih (`abandonAudioFocus`+`stopPlayout`, focus stack kosong) |
| Stop→Start berulang | fase C1 test client ×3 | **PASS 3/3** (`TEST LULUS`, audio re-attach tiap siklus) |

Di-skip (tidak dapat diotomasi di lingkungan ini): lip-sync glass-to-glass
(butuh kamera), runtime Windows (tanpa host), sleep/wake (mengganggu harness),
ganti default output audio (hanya ada 1 output device di host).

## Development — Polish & Release Gate (R6)

Aksesibilitas/robustness yang bisa diverifikasi lokal sudah dikerjakan; yang
butuh pengguna/perangkat khusus dicatat jujur (tidak dicentang).

### Audit kontras (terukur, bukan klaim)

Skrip perbandingan WCAG dijalankan atas token sender + warna inline receiver.
Semua teks bermakna lolos AA (≥4.5); **satu pelanggaran nyata** ditemukan dan
diperbaiki:

| Elemen | Sebelum | Sesudah | Rasio |
|---|---|---:|---:|
| Hint input receiver (placeholder di fill `#0E1D2D`) | `#61788B` 3.71 | `#93A9BC` | **7.01** |

Contoh rasio lain: muted sender 6.24–7.15 · mist/cyan/amber/blue 7.3–16.5 ·
receiver panel text 5.08–14.9. Warna `disabled` (4.37) dikecualikan sesuai WCAG.

### Aksesibilitas & ukuran target

- **Fokus**: sender menambah outline untuk baris opsi & segmen audio
  (`:has(input:focus-visible)`); receiver fokus = fill terang + stroke 3dp +
  teks gelap (bukan hanya warna) — **diverifikasi di MiTV**.
- **Target**: sender tombol/ikon interaktif ≥44 px; receiver tombol/input ≥56 dp.
- **Reduced motion**: sender punya `@media (prefers-reduced-motion: reduce)`.
- **Diagnostic bundle lokal**: dialog Diagnostik → "Ekspor berkas diagnostik"
  (`export_diagnostics`) menulis JSON ke app-data. Bundle **tanpa token/IP/
  kredensial** (dijamin unit test), hanya state teknis. Tidak ada pengiriman
  ke cloud.

### Font scaling & teks panjang (MiTV, terverifikasi)

| font_scale | Hasil |
|---|---|
| 1.0× | normal |
| 1.3× (maks standar Android) | seluruh teks terbaca; deskripsi ber-ellipsis rapi |
| 1.5× (ekstrem) | degradasi rapi (`maxLines`+`ellipsize`), panel interaktif utuh, tanpa potongan tengah-baris |

Tinggi tetap → `minHeight` + `wrap_content` pada tombol/input; nama device
panjang di-ellipsis.

### Belum divalidasi (jujur)

- **TalkBack** di device (label sudah berbasis teks, belum diuji dengan
  screen reader aktif).
- **Usability test** 4/5 user first-connect ≤60 dtk (butuh pengguna nyata).
- **Lip-sync** glass-to-glass (butuh kamera; tercatat di R4).



- **Drift audio**: nol di sisi sender — `sent_ms == packets × 20 ms` eksak
  (mis. 1246 paket = 24920 ms). RTP timestamp dihitung dari akumulasi durasi.
- **Lip-sync glass-to-glass**: belum diukur (butuh kamera sebagai ground truth
  panel). Tidak diklaim ≤150 ms. Sisi sender tidak drift; penyelarasan A/V
  runtime ditangani libwebrtc (RTP timestamp + RTCP SR).

### Status validasi

- **macOS**: capture + encode + E2E ke TV **tervalidasi runtime**.
- **Windows**: WASAPI loopback terimplementasi penuh; **kompilasi diverifikasi**
  (`cargo check --target x86_64-pc-windows-msvc`) tetapi **belum divalidasi
  runtime** (tidak ada host Windows) — perlu build + uji native sebelum dicentang.
- **Perubahan device audio saat sesi aktif** & **sleep/wake**: recovery
  diimplementasikan (rebuild capturer + backoff) tetapi belum diuji dengan
  perangkat berubah saat sesi berjalan.

