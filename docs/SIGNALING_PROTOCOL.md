# WDT Signaling Protocol — v1

Kontrak signaling antara **sender app** (laptop, embed server) dan
**receiver app** (Android TV). Dokumen ini adalah referensi implementasi
(T5 Kotlin) — single source of truth kode ada di
`core/src/signaling/protocol.rs`; keduanya harus sinkron.

- Transport: WebSocket, endpoint `ws://<sender-ip>:8420/ws`
- Encoding: JSON text frame, satu pesan per frame
- Discriminator: field `"type"` (camelCase untuk semua field)
- Versi protokol: `proto: 1`

## Peran & topologi

| Peran | Dari mana | Token | Jumlah |
|---|---|---|---|
| `sender` | loopback saja (127.0.0.1/::1) | tanpa token | maks 1 |
| `receiver` | LAN mana pun | **wajib** token pairing 6-digit | maks 1 aktif (MVP) |

- Token 6-digit numeric; sender app mempersist token di app-data sehingga
  restart **tidak** mengubah token (tampil di UI + QR `ip:port:token`).
- Receiver kedua yang hello valid saat slot terisi → `error senderBusy`.
- Semua pesan selain `hello` sebelum handshake → `error unexpected`.

## Pesan client → server

### `hello` — handshake
```json
{"type":"hello","role":"receiver","proto":1,"token":"123456","deviceId":"tv-living","caps":{"audio":true}}
{"type":"hello","role":"sender","proto":1}
```
| Field | Tipe | Wajib | Catatan |
|---|---|---|---|
| `role` | `"sender" \| "receiver"` | ya | |
| `proto` | number | ya | selain 1 → `protoMismatch` |
| `token` | string | receiver: ya; sender: tidak | 6 digit |
| `deviceId` | string | tidak | label user-friendly TV |
| `caps` | object | tidak | kemampuan receiver; **absen = receiver lama** |

`caps` (v1, aditif — tidak bump `proto`):
| Field | Tipe | Arti |
|---|---|---|
| `audio` | bool | receiver dapat memutar Opus (AudioTrack + audio focus) |

Receiver lama tidak mengirim `caps` → sender memakai offer **video-only**
(tanpa regresi). Sender lama mengabaikan `caps` (serde default) — aman dua arah.

### `sessionConfig` — konfigurasi audio sesi (sender → receiver)
```json
{"type":"sessionConfig","audio":{"enabled":true,"route":"tv","channels":2,"sampleRate":48000,"codec":"opus"}}
```
| Field | Tipe | Arti |
|---|---|---|
| `audio.enabled` | bool | audio media aktif (route TV/Both). **Receiver tidak boleh mengambil audio focus bila false.** |
| `audio.route` | `"laptop" \| "tv" \| "both" \| "muted"` | route dipilih user |
| `audio.channels` | number | 2 |
| `audio.sampleRate` | number | 48000 |
| `audio.codec` | string | `"opus"` |

Dikirim sender **setelah offer** dan **setiap route berubah** (live switching
tanpa renegosiasi). m=audio selalu dinegosiasikan bila `caps.audio=true`,
sehingga mengganti TV↔Laptop hanya mengaktifkan/menonaktifkan pengiriman
sample — video tidak pernah terputus. Receiver lama mem-parse ini sebagai
tipe tak dikenal dan mengabaikannya.

### `offer` — SDP offer (hanya sender)
```json
{"type":"offer","sdp":"v=0\r\n..."}
```

### `answer` — SDP answer (hanya receiver)
```json
{"type":"answer","sdp":"v=0\r\n..."}
```

### `ice` — trickle ICE (dua arah)
```json
{"type":"ice","candidate":{"candidate":"candidate:1 udp 2130706431 192.168.1.5 50000 typ host","sdpMid":"0","sdpMLineIndex":0}}
```

### `bye` — tutup sesi sopan
```json
{"type":"bye","reason":"user-stop"}
```

## Pesan server → client

### `helloOk`
```json
{"type":"helloOk","proto":1,"server":"wdt/0.1.0"}
```

### `receiverJoined` — receiver baru masuk (ke sender)
```json
{"type":"receiverJoined","deviceId":"tv-living","caps":{"audio":true}}
```
Dikirim ke sender setiap receiver berhasil hello. Dipakai UI sender
untuk menampilkan TV yang siap. Sender sebaiknya menunggu pesan ini
sebelum mengirim `offer` (offer tanpa receiver aktif ditolak
`unexpected`). `caps` diteruskan apa adanya dari hello (absen = lama).

### `offer` / `answer` / `ice` / `sessionConfig` — relay ke lawan
Format sama dengan versi client.

### `error`
```json
{"type":"error","code":"badToken","message":"token tidak cocok"}
```
| `code` | Arti | Koneksi ditutup? |
|---|---|---|
| `badToken` | token salah/absen | ya |
| `senderTaken` | slot sender sudah dipakai | ya |
| `senderBusy` | (tidak lagi dikirim) receiver baru kini MENGGANTIKAN receiver lama (`bye replaced`); kode dipertahankan untuk kompatibilitas | — |
| `protoMismatch` | `proto` berbeda | ya |
| `unexpected` | pesan tidak valid untuk state/role | tidak selalu |
| `internalError` | kesalahan server | ya |

### `bye` — lawan disconnect / akhir mirror
```json
{"type":"bye","reason":"peerDisconnected"}
```
`reason` umum:
| `reason` | Arti | Respons receiver yang diharapkan |
|---|---|---|
| `mirror-stop` | sender berhenti mirroring (tombol Stop) — **sesi signaling tetap hidup** | buang peer, re-arm; JANGAN tutup WS (offer berikutnya langsung diproses) |
| `replaced` | receiver baru menggantikan slot ini (last-receiver-wins) | kembali ke layar scan |
| `peerDisconnected` | lawan benar-benar putus | kembali ke layar scan |
| `user-stop` | shutdown sesi (warisan; kini `mirror-stop` untuk Stop mirroring) | kembali ke layar scan |

## Routing audio (R4)

| Route | Media audio ke TV | Output lokal OS |
|---|---|---|
| `laptop` | tidak | tidak diubah |
| `tv` | ya | tidak diubah (local mute tidak dijamin API publik) |
| `both` | ya | tidak diubah |
| `muted` | tidak | tidak diubah |

- Sumber konten = **audio sistem sender** (bukan mikrofon).
- Codec: **Opus 48 kHz stereo**, paket 20 ms. RTP timestamp dihitung
  webrtc-rs dari akumulasi durasi sample → monotonik tanpa drift.
- Negosiasi: `m=audio` disertakan sejak offer bila `caps.audio=true`;
  route hanya mengatur pengiriman sample (`sessionConfig.audio.enabled`).
- Kompatibilitas: receiver lama (tanpa `caps`) → offer video-only,
  perilaku R3 tidak berubah. Tidak ada bump `proto`.

## Flow normal (happy path)

```
Sender(loopback)                Server                   Receiver(LAN)
      |-- hello{sender} -------->|                            |
      |<------------- helloOk ---|                            |
      |                          |<------ hello{token} -------|
      |                          |--- helloOk --------------->|  (token divalidasi)
      |-- offer{sdp} ----------->|--- offer{sdp} ------------>|
      |<-------------------------|<------ answer{sdp} --------|
      |-- ice{cand} ------------>|--- ice{cand} ------------->|
      |<-------------------------|<------ ice{cand} ----------|
      |        ... WebRTC P2P terbentuk (DTLS/SRTP) ...       |
```

Catatan: sender selalu offerer (sesuai T3). Trickle ICE: kirim
kandidat segera setelah answer di-set; kandidat sebelum answer di antre
oleh kedua sisi.

## mDNS (discovery otomatis)

- Service type: `_wdt._tcp.local.`
- Instance name: `WDT <hostname>` (≤ 15 karakter, mis. `WDT MacBook-Pro`)
- TXT record:

| Key | Contoh | Arti |
|---|---|---|
| `proto` | `1` | versi protokol signaling |
| `ver` | `0.1.0` | versi sender app |

- Port ada di record SRV; IP di record A/AAAA. Receiver Android memakai
  NSD API dengan bentuk service type **tanpa domain**: `_wdt._tcp`
  (`NsdManager.discoverServices("_wdt._tcp", PROTOCOL_DNS_SD)`); bentuk
  `_wdt._tcp.local.` adalah notasi DNS-SD penuh yang dipakai advertiser
  (`mdns-sd` di Rust). Keduanya merujuk service yang sama.
- Token pairing **tidak** di-advertise di TXT record (hanya `proto`/`ver`)
  supaya tidak bocor ke seluruh LAN; receiver tetap harus memasukkannya
  (dari layar sender atau QR).

## Fallback manual (mDNS diblokir)

Receiver menyediakan form input `ip:port:token` (atau scan QR berisi
string yang sama). Format pairing: `192.168.1.5:8420:123456`.
