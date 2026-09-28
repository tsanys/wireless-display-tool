# Rilis (R9)

Panduan membuat rilis GitHub berisi tiga artefak:

| Artefak | Platform | Format | Job CI |
|---|---|---|---|
| `wdt-sender-<tag>-macos-universal.dmg` | macOS (arm64 + x86_64) | Tauri `.dmg` | `macos` |
| `wdt-sender-<tag>-windows-x64-setup.exe` | Windows x64 | Tauri NSIS | `windows` |
| `wdt-receiver-<tag>-android.apk` | Android TV | Gradle `assembleDebug` | `android` |

> **Status signing: UNSIGNED / adhoc.** Artefak awal belum ditandatangani
> (macOS tanpa notarization, Windows tanpa sertifikat, Android memakai debug
> keystore). Lihat bagian *Signing* di bawah untuk konsekuensinya.

## Cara membuat rilis

Workflow: `.github/workflows/release.yml`.

### Opsi A — tag (jalur normal)

```bash
git tag v0.1.0
git push origin v0.1.0
```

Workflow berjalan untuk tag `v*`, membangun ketiga artefak, lalu membuat
GitHub Release bertajuk `Wireless Display Tool v0.1.0` dengan catatan rilis
otomatis (`--generate-notes`).

### Opsi B — manual (uji coba sebelum tag)

Jalankan `Release` via **Actions → Release → Run workflow** dan isi `tag`
(mis. `v0.1.0-rc1`). Gunakan ini untuk memverifikasi ketiga job hijau tanpa
membuat tag permanen. Jika Release dengan tag tersebut sudah ada, artefak akan
diunggah ulang (`--clobber`).

## Konsekuensi artefak unsigned

- **macOS**: pengguna harus klik-kanan → *Open* (atau
  `xattr -dr com.apple.quarantine`) karena Gatekeeper memblokir aplikasi tanpa
  notarization. Untuk menghilangkannya perlu Apple Developer ID + notarization
  (`APPLE_CERTIFICATE`, `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_PASSWORD`).
- **Windows**: SmartScreen menampilkan peringatan "Windows protected your PC" →
  *More info* → *Run anyway*. Untuk menghilangkannya perlu sertifikat
  code-signing (`tauri.conf.json` → `bundle.windows.certificateThumbprint`
  atau secrets di CI).
- **Android**: APK ditandatangani **debug keystore** sehingga dapat dipasang
  langsung (sideload), tetapi tidak untuk Play Store dan tidak dapat
  di-`update` dengan build ber-keystore rilis. Untuk rilis produksi perlu
  keystore + `signingConfigs` di `receiver-app/app/build.gradle.kts` dan secret
  CI (`ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, dll).

## Menyiapkan signing (nanti)

1. **macOS**: buat Developer ID Application cert, ekspor `.p12`, tambahkan
   sebagai secret GitHub; Tauri menandatangani otomatis bila env
   `APPLE_CERTIFICATE`/`APPLE_SIGNING_IDENTITY` tersedia, lalu notarize via
   `APPLE_ID`/`APPLE_PASSWORD`/`APPLE_TEAM_ID`.
2. **Windows**: beli sertifikat EV/OV, simpan di runner (atau gunakan
   `tauri-apps/tauri-action` dengan secret PFX), set thumbprint.
3. **Android**: `keytool -genkeypair` → keystore; simpan base64 di secret;
   tambahkan `signingConfigs.release` + `buildTypes.release.signingConfig`.

Setelah signing aktif, perbarui tabel di atas dan hapus catatan unsigned.

## Versi

Versi saat ini ada di tiga tempat — samakan sebelum tag:

- `sender-app/package.json` → `version`
- `sender-app/src-tauri/tauri.conf.json` → `version`
- `receiver-app/app/build.gradle.kts` → `versionName` (dan `versionCode` += 1)

## Verifikasi manual artefak

- **.dmg**: buka di macOS, pastikan aplikasi berjalan dan dapat menemukan TV
  (butuh izin Screen Recording + Local Network).
- **.exe**: jalankan installer di Windows, pastikan shortcut dibuat dan app
  terbuka (butuh Windows Media Foundation untuk capture).
- **.apk**: `adb install -r wdt-receiver-<tag>-android.apk` di Android TV,
  pastikan app terbuka dan menampilkan layar setup.
