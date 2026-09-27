# R5 — Keputusan Extended Display (Riset & PoC)

Status: **R5a selesai** (riset + PoC + keputusan). **R5b macOS diimplementasikan
(eksperimental)**. R5b Windows belum (terblokir host).

## Ringkasan keputusan

| Platform | Keputusan | Dasar |
|---|---|---|
| **macOS** | **GO (eksperimental)** — diimplementasikan di R5b | PoC + E2E device semua assertion PASS |
| **Windows** | **GO via driver IddCx Rust sendiri** (belum diimplementasi) | kontrol lifecycle penuh; signing sebagai gerbang rilis |

## R5b macOS — implementasi (eksperimental)

Modul: `core/src/vdisplay/` (`mod.rs` + `macos.rs`).
- `capability()` — cek keberadaan kelas privat (tanpa efek samping).
- `VirtualDisplay::create(w,h,hz)` + `capture_id()` + `Drop` (lepas bersih).
- Guard main display: restore langsung bila display virtual merebut slot main,
  plus observer `NSApplicationDidChangeScreenParameters` (best-effort, main queue).
- Integrasi: `CaptureTarget::{Display,Virtual}` di `sender_session`; pipeline
  membuat display virtual di thread capture dan melepasnya saat Stop.
- Capability UI: `virtual_display` berasal dari probe nyata; opsi "Layar
  tambahan" aktif + label **Eksperimental** (badge "Segera" disembunyikan).

### Bukti
- `cargo run -p wdt-core --example virtual_display_probe` → **SEMUA PASS**
  (displayID dibuat, terlihat CG+WDT+SCK, 5 frame 1920×1080, lepas bersih).
- E2E device (MiTV): `WDT_EXTENDED=1920x1080` → macOS menampilkan 2 display
  (built-in 3024×1964 + virtual 1920×1080); TV menampilkan **desktop display
  virtual** (menu bar, 16:9 penuh); audio ~96 kbps; `Stop` → display virtual
  dilepas (kembali 1 display, tanpa yatim).

### Limitasi jujur
- **Konten statis → fps rendah**: ScreenCaptureKit hanya mengirim frame saat ada
  perubahan, jadi display virtual yang kosong ~5–9 fps (bukan bug; konten yang
  bergerak tetap ~29 fps di mirror). Untuk merekam layar diam, tidak ada
  perubahan berarti memang tidak ada frame baru.
- Observer hot-plug **best-effort** (main queue); kegagalan tidak fatal.
- API privat: bisa berubah pada major release macOS → capability probe +
  pesan error jelas (bukan crash). Fitur default dipilih eksplisit user.
- Belum diuji: sleep/wake dan perpindahan kabel/display saat Extended aktif.

---

## Bagian 1 — macOS (riset & PoC)

### 1.1 Tidak ada API publik (terverifikasi)

Dicek langsung terhadap SDK terpasang (`xcrun --show-sdk-path`, macOS 27):

```
CoreGraphics headers publik : TIDAK ADA API virtual display
ScreenCaptureKit headers    : TIDAK ADA API "membuat" display (hanya capture)
DriverKit                   : TIDAK ADA display family untuk pihak ketiga
```

Konsensus ekosistem (pencarian GitHub "virtual display mac": 83 repo) juga
menunjukkan hal sama: **semua** proyek bermakna memakai kelas privat
`CGVirtualDisplay` (BetterDisplay 33,8k★, FluffyDisplay, Crisp 1,9k★,
MacVirtualDisplay, node-mac-virtual-display, dll). Tidak ada jalur publik yang
terlewat.

### 1.2 PoC dijalankan — HASIL: SEMUA PASS

`core/examples/virtual_display_probe.rs` membuat display 1920×1080@60 via
`CGVirtualDisplayDescriptor` → `CGVirtualDisplay` → `applySettings`, lalu
memverifikasi berurutan. Hasil pada host uji:

```
main display sebelum : 1
LANGKAH 1 PASS: display dibuat, displayID=5
LANGKAH 2 PASS: terlihat oleh CG + WDT            (active_displays=true wdt_available=true)
LANGKAH 3 PASS: terlihat oleh ScreenCaptureKit    (interop kritis — LOLOS)
LANGKAH 4 PASS: 5 frame tertangkap                (1920x1080, 8.294.400 byte/frame)
LANGKAH 5 PASS: dilepas bersih                    (hilang_dari_daftar=true main_display_sama=true)
== HASIL: SEMUA PASS — GO untuk Extended macOS (eksperimental) ==
```

Poin penting: **laporan interop** bahwa display virtual tak muncul di
`SCShareableContent` (issue opendisplay #142, macOS 15.7) **tidak tereproduksi**
di host ini — SCK melihat display virtual dan pipeline capture WDT yang ada
langsung menangkap frame darinya. Pelepasan bersih (tidak ada display yatim;
main display tidak berubah).

### 1.3 Risiko & mitigasi

| Risiko | Tingkat | Mitigasi |
|---|---|---|
| API privat berubah di major release macOS | Sedang | Isolasi di satu modul (`vdisplay/macos`), capability probe saat runtime (coba buat+lepas display), toggle eksperimental default OFF, fallback dokumentasi dummy-plug |
| Display virtual sempat jadi **main display** / masuk mirror set | Sedang | Referensi MIT menangani ini via `CGConfigureDisplayOrigin` + observer `NSApplicationDidChangeScreenParameters`; PoC sudah memverifikasi main display tetap (langkah 5) — R5b wajib port logika guard |
| Symbol privat tak resolve di bawah hardened runtime | Rendah | `objc_getClass` runtime lookup; probe melaporkan jelas bila gagal (bukan crash) |
| Distribusi (App Store) | Tidak relevan | WDT didistribusikan langsung (Tauri), bukan App Store |

### 1.4 Catatan lisensi (penting)

- Referensi API shape: **node-mac-virtual-display (MIT)** — boleh dirujuk/diadaptasi.
- **rustscreen: lisensi belum final (all rights reserved)** — kode TIDAK disalin;
  hanya dipakai sebagai bukti kelayakan.
- Binding kita ditulis sendiri via `objc2` `msg_send` ke kelas privat; tidak ada
  kode pihak ketiga yang di-vendor.

---

## Bagian 2 — Windows

### 2.1 Strategi driver

Perbandingan IDD (dari README proyek terkait):

| Proyek | IddCx | Signed | Controller | Bahasa |
|---|---|---|---|---|
| `virtual-display-rs` | 1.5 | ❌ | ✅ | Rust |
| `RustDeskIddDriver` | 1.2 | ❌ | 🆗 | Rust |
| `Virtual-Display-Driver (HDR)` | 1.10 | ✅ | ❌ | C++ |
| `parsec-vdd` | 1.5 | ✅ | ✅ | proprietary |

Kebutuhan inti (blueprint R5): "virtual display wajib dilepas tanpa
meninggalkan konfigurasi rusak" + persist per-TV ⇒ butuh **plug/unplug + set
mode programatik**. Hanya opsi yang punya *controller* (virtual-display-rs,
parsec-vdd). Tidak memakai driver proprietary pihak ketiga ⇒ **basis:
`virtual-display-rs`** (Rust, IddCx 1.5, controller), di-fork/diadaptasi.

### 2.2 Fase & gerbang

1. **R5b-dev**: fork/adapt driver, instal dengan **test-signing** di host
   Windows; validasi lifecycle (create/remove, modes, capture oleh pipeline WDT,
   cleanup saat Stop/crash/sleep-wake/device-change).
2. **Gerbang rilis user**: Extended Windows baru diaktifkan setelah
   **attestation signing** (EV cert + Hardware Dev Center). Sebelum itu,
   capability = "butuh mode pengembang" (opsi disabled untuk user normal).
3. **Opsional (terpisah)**: deteksi driver signed yang sudah terpasang di mesin
   user sebagai backend kompatibilitas. Tidak memblokir R5b.

**Beban/ops yang harus disadari:** EV code-signing cert (~biaya tahunan) +
proses attestation Microsoft. Karena itu dipisah sebagai gerbang rilis.

### 2.3 Runbook validasi (dijalankan saat host Windows tersedia)

> Tidak ada host Windows di lingkungan pengembangan saat R5a. Semua item di
> bawah berstatus **belum dijalankan**.

Sekaligus menutup utang validasi **R4 Windows**:

1. `bcdedit /set testsigning on` + reboot; verifikasi watermark test mode.
2. `pnputil /add-driver <inf> /install` — driver virtual display muncul.
3. `cargo check --target x86_64-pc-windows-msvc` (sudah hijau dari macOS) →
   `cargo test --workspace` di Windows (build + unit asli).
4. **R4 audio WASAPI**: jalankan sender, putar audio sistem, verifikasi
   `sent_ms == packets×20` dan capture menghasilkan sampel.
5. **R4 encoder MF**: `capture_encode_test` → ffmpeg decode; soak
   `pipeline_soak` (RSS datar? — perbaikan reuse buffer Windows belum
   divalidasi runtime).
6. **R5 driver**: create/remove virtual display via IOCTL; muncul di
   `available_displays()`; capture; cleanup setelah crash/Stop; sleep/wake.

---

## Langkah berikutnya

1. R5b macOS (eksperimental, toggle OFF): port logika guard main-display +
   lifecycle (create on Extended start, remove on Stop), capability probe.
2. R5b Windows: fork `virtual-display-rs`, wiring IOCTL, installer flow —
   diblokir validasi sampai host Windows ada.
3. R6 (polish) tetap dapat dikerjakan paralel tanpa host Windows.
