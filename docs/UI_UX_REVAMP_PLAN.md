# Blueprint Revamp Sender & Receiver

Dokumen ini menjadi spesifikasi produk, UX, dan tahapan implementasi untuk
membuat Wireless Display Tool terasa profesional, mudah dipahami, dan tetap
jujur terhadap kemampuan platform. Implementasi dilakukan bertahap; opsi yang
belum didukung backend **tidak boleh terlihat seolah-olah sudah berfungsi**.

## 1. Keputusan produk

Alur utama sender cukup menjawab empat pertanyaan:

1. TV mana yang akan dipakai?
2. Layar akan dicerminkan atau dijadikan layar tambahan?
3. Suara diputar di mana?
4. Siap mulai berbagi?

Istilah user-facing:

- `Receiver` menjadi **TV** atau **layar tujuan**.
- `Mirror` menjadi **Cerminkan layar**.
- `Extended` menjadi **Layar tambahan**.
- `Audio source` tidak dipakai karena ambigu. Kontrol utamanya bernama
  **Suara diputar di**: `Laptop`, `TV`, `Keduanya`, atau `Tanpa suara`.
- `Signaling`, `offer`, `ICE`, `profile`, dan alamat IP hanya muncul di
  **Diagnostik lanjutan**.

Asumsi produk untuk audio: user memilih **tempat suara diputar**, bukan sumber
konten audio. Sumber awal adalah audio sistem laptop. Mikrofon adalah opsi
terpisah di fase lanjut agar tidak tercampur dengan routing output.

## 2. Arah visual: “Living-room link”

Identitas visual mengambil metafora hubungan laptop ke layar ruang keluarga:
dua perangkat dihubungkan oleh jalur sinyal yang menunjukkan kondisi nyata.
Ini menjadi elemen khas aplikasi, bukan dashboard SaaS berisi banyak kartu.

Token awal:

| Token | Nilai | Pemakaian |
|---|---:|---|
| Ink | `#0B1320` | latar utama |
| Slate | `#132238` | panel dan permukaan sekunder |
| Mist | `#EAF2F8` | teks utama |
| Signal blue | `#46A7FF` | aksi utama dan fokus |
| Link cyan | `#52D3C6` | koneksi sehat/aktif |
| Warning amber | `#F2B84B` | butuh perhatian |

- Sender: `Manrope` yang dibundel lokal, fallback ke system sans-serif.
- Receiver: Android system sans/Roboto untuk footprint kecil dan rendering
  konsisten di Android TV low-end.
- Radius sedang (10–14 px), garis tipis, bayangan minim. Status tidak hanya
  dibedakan dengan warna: selalu ada ikon dan teks.
- Animasi hanya menjelaskan perubahan state. Jalur sinyal berdenyut saat
  menghubungkan, diam saat tersambung, dan berhenti pada reduced motion.

## 3. Information architecture sender

Sender memakai satu workspace, bukan empat kartu teknis bertumpuk.

```text
┌──────────────────────────────────────────────────────────────┐
│ WDT                  Siap digunakan             ⚙ Bantuan    │
├───────────────────┬──────────────────────────────────────────┤
│ 1  TV tujuan      │                                          │
│    Ruang Keluarga │       [ Laptop ] ─────── [ TV ]          │
│                   │          jalur status koneksi             │
│ 2  Tampilan       │                                          │
│    ● Cerminkan    │  Ringkasan                               │
│    ○ Tambahan     │  TV Ruang Keluarga · 1080p · Suara di TV │
│                   │                                          │
│ 3  Suara          │                       [Mulai berbagi]     │
│    ● TV           │                                          │
└───────────────────┴──────────────────────────────────────────┘
```

### State utama

| State | Judul | Aksi primer | Detail yang terlihat |
|---|---|---|---|
| Belum ada TV | **Hubungkan TV** | Tampilkan kode | langkah singkat, QR/token |
| Siap | **Siap berbagi** | Mulai berbagi | TV, mode, audio, kualitas |
| Menghubungkan | **Menghubungkan ke TV…** | Batalkan | progres tahap yang manusiawi |
| Aktif | **Layar sedang dibagikan** | Berhenti | durasi, resolusi, audio |
| Bermasalah | **Belum dapat terhubung** | Coba lagi | penyebab + satu tindakan |

Aturan interaksi:

- Satu TV yang tersedia dipilih otomatis; lebih dari satu tetap meminta
  pilihan eksplisit.
- Pengaturan tetap bisa diperiksa sebelum mulai, tetapi dikunci saat sesi
  aktif kecuali kontrol yang aman seperti mute/routing audio.
- Tombol utama persisten dan selalu menjelaskan hasil tindakannya.
- QR, token, firewall, IP, codec, bitrate, dan statistik dipindahkan ke drawer
  **Hubungkan secara manual** / **Diagnostik**.
- Error harus memasangkan masalah dengan tindakan, misalnya:
  “TV dan laptop mungkin berada di jaringan berbeda” + “Buka panduan”.

### Pengaturan tampilan

- **Cerminkan layar**: pilih display fisik bila laptop mempunyai lebih dari
  satu layar. Ini menjadi default.
- **Layar tambahan**: buat virtual display lalu tampilkan di TV. Opsi hanya
  aktif bila capability backend menyatakan tersedia.
- Jika driver/permission belum tersedia, tampilkan status jujur seperti
  “Perlu memasang komponen layar tambahan” beserta aksi setup; jangan diam-
  diam fallback ke mirror.
- Advanced: resolusi `Otomatis`, `1080p`, `720p`; frame rate `Otomatis`,
  `30 fps`, `60 fps` bila perangkat dan jaringan mendukung.

### Pengaturan suara

| Pilihan UI | Perilaku |
|---|---|
| Laptop | audio lokal tetap hidup; track audio ke TV tidak dikirim |
| TV | audio sistem dikirim ke TV; local mute bersifat best-effort per OS |
| Keduanya | audio lokal tetap hidup dan track juga dikirim ke TV |
| Tanpa suara | tidak ada track audio dan output lokal tidak diubah |

Untuk `Keduanya`, tampilkan catatan bahwa dua perangkat dapat terdengar tidak
sinkron karena jarak dan latensi jaringan. Jika OS tidak dapat mematikan suara
lokal secara aman pada mode `TV`, UI menyatakan “Matikan volume laptop bila
masih terdengar”, bukan mengklaim mute berhasil.

## 4. Information architecture receiver TV

Receiver dioptimalkan untuk remote/D-pad dan jarak pandang ruang keluarga.

```text
┌──────────────────────────────────────────────────────────────┐
│ WDT                                                          │
│                                                              │
│             Siap menerima layar                              │
│       Buka WDT di laptop pada jaringan yang sama             │
│                                                              │
│       Laptop Pandu                         [Hubungkan]        │
│                                                              │
│       Tidak terlihat?  Hubungkan dengan kode                 │
└──────────────────────────────────────────────────────────────┘
```

- First run: maksimal tiga instruksi, daftar laptop yang ditemukan, serta
  fallback kode manual. Alamat IP dan versi tidak muncul di label perangkat.
- Token memakai enam kotak digit besar atau keyboard numerik, bukan input
  gabungan `ip:port:token` sebagai jalur utama.
- Target fokus minimum 56 dp; fokus D-pad memakai outline blue yang jelas,
  bukan hanya perubahan warna halus.
- Semua konten setup berada dalam safe area untuk overscan TV.
- Saat video tampil, setup hilang dan overlay status otomatis menghilang
  setelah 4 detik. Tombol OK menampilkan panel sesi; Back meminta konfirmasi
  sebelum memutus.
- Panel sesi berisi nama laptop, `1920×1080 · 30 fps`, lokasi audio, kualitas
  koneksi sederhana (`Baik`, `Kurang stabil`), dan tombol Putuskan.
- Statistik teknis lengkap hanya ada pada mode diagnostik.

## 5. Kontrak model dan capability

Model yang disarankan di core/Tauri:

```rust
enum DisplayMode {
    Mirror { display_id: String },
    Extended { resolution: Resolution, refresh_hz: u32 },
}

enum AudioRoute { Laptop, Tv, Both, Muted }
enum AudioCaptureSource { System /* Microphone menyusul */ }

struct ShareSettings {
    receiver_id: String,
    display_mode: DisplayMode,
    audio_route: AudioRoute,
    audio_source: AudioCaptureSource,
    quality: QualityPreset,
}

struct CapabilityReport {
    displays: Vec<DisplayInfo>,
    virtual_display: CapabilityState,
    system_audio_capture: CapabilityState,
    receiver_audio: bool,
}
```

Command Tauri berubah dari `start_mirroring(receiver_id)` menjadi
`start_sharing(settings)`. Backend memvalidasi kembali capability—UI bukan
batas keamanan. Pengaturan dipersist per TV, tetapi setiap sesi tetap memakai
snapshot immutable supaya UI dan pipeline tidak berbeda state.

Signaling perlu versi protokol baru untuk capability + `SessionConfig`
(display kind, audio enabled, channel count, sample rate). Receiver lama harus
tetap menerima video-only atau ditolak dengan pesan upgrade yang jelas, bukan
gagal negosiasi tanpa penjelasan.

## 6. Arsitektur implementasi fitur

### Mirror dan pemilihan layar

1. Ubah `ScreenCapturer` agar dapat melakukan enumerate display dan menerima
   `display_id`, bukan selalu mengambil display utama.
2. Tampilkan thumbnail/nama display di sender.
3. Pertahankan letterbox + quality pipeline yang sudah ada.

### Extended display

1. Buat PoC terpisah dan capability gate.
2. Windows: virtual display berbasis IddCx; perhitungkan driver packaging,
   test-signing saat development, dan production signing.
3. macOS: validasi API, entitlement, kompatibilitas versi, dan kebijakan
   distribusi untuk virtual display sebelum menetapkan implementasi. Jangan
   mengunci desain ke API privat sebelum PoC lolos.
4. Virtual display yang berhasil dibuat masuk ke daftar capture source normal.
5. Lifecycle harus tahan sleep, kabel/display berubah, crash, dan Stop; virtual
   display wajib dilepas tanpa meninggalkan konfigurasi display rusak.

### Audio ke TV

```text
system audio capture ──> resample 48 kHz ──> WebRTC Opus track
                                              │ A/V timestamps
                                              ▼
                                    Android WebRTC AudioTrack
```

- macOS: PoC system audio dengan ScreenCaptureKit pada versi OS target.
- Windows: WASAPI loopback capture.
- Transport: Opus 48 kHz, stereo bila receiver mendukung; gunakan timestamp
  satu clock domain dengan video agar lip-sync dapat diukur.
- Receiver: audio focus, volume ducking yang benar, dan cleanup saat Stop.
- Jangan mengirim audio untuk rute `Laptop`/`Muted`; ini menghemat bandwidth
  dan menghindari state mute yang membingungkan.

## 7. Tahapan delivery

### R0 — Kontrak UX dan capability (1 milestone)

- [x] Tambah `ShareSettings`, `CapabilityReport`, dan snapshot state sesi.
- [x] Pisahkan copy user-facing dari pesan diagnostik.
- [x] Tambah mock capability/state untuk UI development dan screenshot states.
- [ ] Bekukan wireframe serta copy Indonesia; siapkan English localization.

### R1 — Sender redesign, fitur saat ini (1 milestone)

- [x] Implement shell, device link visual, setup rail, dan session view.
- [x] Hubungkan Mirror + video-only yang sudah ada tanpa mengubah pipeline.
- [x] Pindahkan pairing/firewall/statistik ke dialog sekunder.
- [x] Lengkapi loading, empty, reconnect, permission, dan error states.

### R2 — Receiver redesign (1 milestone)

- [x] Implement layout TV safe-area dan D-pad focus order.
- [x] Perbaiki pairing digit, discovered-device list, dan recovery copy.
- [x] Auto-hide overlay saat playback; tambah session panel on-demand.
- [x] Uji setup/pairing di Xiaomi MiTV-MOOR2 (1080p, API 30) dengan keyevent
  remote: safe-area/overscan, urutan fokus, fallback manual, dan konfirmasi Back.
- [x] Uji playback aktual di TV: first frame, statistik runtime, auto-hide 4
  detik, panel OK, dan tombol tutup.
- [x] Uji recovery setelah Stop/Start sender tanpa pairing ulang.

### R3 — Multi-display Mirror (1 milestone)

- [x] Enumerate display dan capture by ID pada macOS/Windows.
- [x] Tambah picker + persist pilihan per-TV dengan fallback informatif saat
  display yang tersimpan sudah tidak terhubung.
- [x] Uji sender nyata → Xiaomi MiTV-MOOR2 memakai ID display macOS: first
  frame tampil dan stabil 29–30 fps; uji picker dua-display, persist setelah
  reload, dan hot-unplug fallback dilakukan lewat mock capability interaktif.

### R4 — Audio routing (2 milestone platform + integrasi)

- [x] Pipeline audio core: normalisasi PCM → 48 kHz stereo → Opus 20 ms →
  audio track WebRTC; route watch + backpressure bounded; timestamp monotonik.
- [x] **macOS**: capture audio sistem ScreenCaptureKit (SCStream audio-only 48 kHz
  stereo) — **tervalidasi runtime** di host (`audio_capture_probe`: ±147 frame /
  3 dtk, peak 0.198) dan E2E ke MiTV-MOOR2 (audio 28–69 kbps ter-decode/playout).
- [x] Windows: implementasi WASAPI loopback (COM MTA, default render endpoint,
  shared mode, recovery device invalidation, stereo f32). **Kompilasi
  diverifikasi** via `cargo check --target x86_64-pc-windows-msvc`; **belum
  divalidasi runtime** (tidak ada host Windows).
- [x] WebRTC: Opus + H.264 didaftarkan eksplisit; PT/SSRC dipilih per media kind
  (bukan sender pertama); offer AV dua m-line (video+audio).
- [x] Receiver Android: AudioTrack (onTrack + onAddTrack), enable/disable per
  SessionConfig, AudioManager audio focus (request/abandon), cleanup saat
  stop/disconnect/teardown.
- [x] Signaling: `caps` opsional pada hello/receiverJoined + `sessionConfig`
  (aditif, backward-compatible, proto tetap v1); wire test Rust + Kotlin.
- [x] UI sender: opsi **Tanpa suara**; TV/Keduanya aktif hanya bila capability
  nyata (capture + receiver audio); persist per-TV; live switching tanpa putus
  video; warning jujur (TV: "matikan volume laptop bila masih terdengar";
  Keduanya: potensi tidak sinkron).
- [x] Uji device MiTV-MOOR2 (ADB): route Laptop/TV/Keduanya/Tanpa suara, live
  switching (Tv→Laptop→Both→Tv), recovery Stop→Start tanpa pairing ulang,
  teardown bersih (`abandonAudioFocus` + `stopPlayout`).
- [ ] Lip-sync glass-to-glass terukur (≤150 ms): **belum diukur** — butuh kamera
  sebagai ground truth panel. Sisi sender terbukti tanpa drift (sent_ms ==
  packets × 20 ms eksak). Metode + angka CPU sudah dicatat di README.
- [ ] Perubahan device audio (ganti default output) & sleep/wake pada perangkat
  nyata: recovery sudah diimplementasikan (rebuild capturer + backoff) tetapi
  **belum diuji** dengan perangkat berubah saat sesi aktif.

### R5 — Extended display (milestone riset lalu implementasi per OS)

Riset + PoC + keputusan terdokumentasi di **`docs/R5_DECISIONS.md`**.

- [x] **Riset per OS**: macOS tidak punya API publik (verifikasi SDK + konsensus
  83 repo → semua pakai `CGVirtualDisplay` privat); Windows punya ekosistem IDD
  (`virtual-display-rs` controller ✅, VDD-HDR signed tapi tanpa controller).
- [x] **PoC macOS dijalankan**: display virtual dibuat via API privat, terlihat
  oleh CG + WDT + **ScreenCaptureKit**, 5 frame tertangkap, dilepas bersih
  (main display utuh). Hasil: **GO eksperimental** (`examples/virtual_display_probe`).
- [x] **Keputusan Windows**: driver IddCx Rust sendiri (basis `virtual-display-rs`),
  fase dev test-signing, gerbang rilis attestation signing. Runbook validasi
  disiapkan untuk saat host Windows tersedia.
- [x] **R5b — implementasi macOS** (eksperimental, capability-gated): modul
  `core/src/vdisplay/` (create/drop + guard main-display), `CaptureTarget::Virtual`
  di pipeline, opsi UI aktif hanya saat probe lulus + label eksperimental.
  Diverifikasi: probe SEMUA PASS + E2E device (TV menampilkan display virtual,
  audio jalan, Stop melepas display tanpa yatim). Lihat `docs/R5_DECISIONS.md`.
- [ ] **R5b — implementasi Windows**: fork driver, wiring IOCTL, installer,
  lifecycle; **terblokir validasi** sampai ada host Windows (test-signing).
- [x] Aktifkan opsi UI Extended hanya pada capability yang lulus (per OS):
  macOS aktif saat probe lulus (+label eksperimental); platform lain menampilkan
  alasan jujur (Windows menunggu driver IddCx).

### R6 — Polish dan release gate

- [x] **D-pad + fokus TV**: indikator fokus kuat (fill terang + stroke cyan +
  teks gelap) diverifikasi di device; traversal D-pad scan→device→panel.
- [x] **Reduced motion** (sender): `@media (prefers-reduced-motion: reduce)`
  menonaktifkan animasi jalur sinyal. Indikator fokus juga ada untuk baris
  opsi/segmen audio (`:has(input:focus-visible)`).
- [x] **Target ukuran**: sender tombol/ikon interaktif ≥44 px; receiver
  tombol/input ≥56 dp (tinggi tetap → `minHeight` agar tahan font besar).
- [x] **Kontras WCAG AA**: audit terukur — satu pelanggaran nyata diperbaiki
  (hint input receiver `#61788B` = **3.71** → `#93A9BC` = **7.01** pada fill
  input). Sisa teks ≥4.5 (disabled dikecualikan WCAG).
- [x] **Font scaling + teks panjang**: diverifikasi di MiTV pada 1.0×/1.3×/1.5×
  (kolom kiri turun ukuran + `maxLines`/`ellipsize`, tidak ada potongan
  tengah-baris); nama device panjang di-ellipsis.
- [x] **Copy error**: sender menambah pemetaan offline/jaringan, kode pairing
  salah, dan TV sedang dipakai (sebelumnya generik).
- [x] **Diagnostic bundle lokal** (bukan telemetri cloud): command
  `export_diagnostics` menulis JSON ke app-data; diuji unit (bundle **tanpa**
  token/IP/secret, round-trip berkas).
- [ ] **Screen reader (TalkBack)**: label berbasis teks sudah memadai, tetapi
  **belum diuji dengan TalkBack aktif** di device — perlu verifikasi manual.
- [ ] **Usability test** (4/5 user first-connect ≤60 dtk): tidak dapat
  diotomasi; perlu sesi pengguna nyata.
- [ ] **Lip-sync glass-to-glass** (≤150 ms): ditunda ke R4 (butuh kamera).

Catatan jujur: item yang tidak dapat divalidasi di lingkungan ini (TalkBack,
usability, lip-sync) **tidak dicentang**.

## 8. Acceptance criteria

- User baru dapat menghubungkan TV dan mulai berbagi tanpa memahami IP,
  signaling, codec, atau WebRTC.
- Dalam usability test, 4 dari 5 user menyelesaikan first connection tanpa
  bantuan dalam ≤60 detik pada jaringan normal.
- Mode, layar tujuan, dan lokasi suara selalu terlihat sebelum Start.
- Opsi unsupported mempunyai alasan dan langkah perbaikan yang eksplisit.
- Semua alur receiver dapat dipakai hanya dengan D-pad, OK, dan Back.
- Overlay tidak menutupi konten saat playback normal.
- Audio TV memiliki lip-sync terukur, reconnect bersih, dan tidak meninggalkan
  audio focus setelah sesi berhenti.
- Extended mode tidak pernah aktif tanpa backend virtual display yang benar-
  benar siap dan lolos lifecycle test.

## 9. Urutan prioritas yang disarankan

Kerjakan `R0 → R1 → R2 → R3 → R4 → R5 → R6`. Redesign tidak perlu menunggu
Extended selesai: R1/R2 menggunakan capability gate sehingga UI baru dapat
dirilis dengan Mirror + video-only, lalu fitur audio dan Extended muncul saat
backend masing-masing benar-benar siap.
