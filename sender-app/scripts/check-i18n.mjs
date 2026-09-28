#!/usr/bin/env node
/**
 * Validasi kamus i18n sender (R8) — tanpa browser/TS.
 *
 * Memastikan: tidak ada nilai kosong, EN berbeda dari ID, tidak ada kunci
 * duplikat (JSON sudah menjamin), dan placeholder `{...}` konsisten antara
 * ID dan EN. Dijalankan di CI sebagai guard.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const dictPath = join(here, "..", "src", "i18n.id-en.json");
const dict = JSON.parse(readFileSync(dictPath, "utf8"));

const errors = [];
const entries = Object.entries(dict);
if (entries.length < 50) errors.push(`kamus terlalu kecil: ${entries.length} entri`);

// Istilah yang memang identik dalam bahasa Indonesia dan Inggris (kognat /
// istilah teknis). EN==ID diizinkan HANYA untuk kunci dalam daftar ini —
// setiap penambahan harus ditinjau, bukan jalan pintas untuk string yang
// belum diterjemahkan.
const IDENTICAL_ALLOWED = new Set(["Audio", "Frame rate", "Server", "Video"]);

const placeholders = (s) => (s.match(/\{[a-zA-Z_]+\}/g) ?? []).sort().join(",");
for (const [id, en] of entries) {
  if (!id.trim()) errors.push("kunci ID kosong");
  if (typeof en !== "string" || !en.trim()) errors.push(`EN kosong untuk: ${id}`);
  if (
    en.trim().toLowerCase() === id.trim().toLowerCase() &&
    !IDENTICAL_ALLOWED.has(id)
  ) {
    errors.push(`EN sama dengan ID (tak diterjemahkan): ${id}`);
  }
  const pi = placeholders(id);
  const pe = placeholders(en);
  if (pi !== pe) errors.push(`placeholder beda untuk "${id}": ID[${pi}] vs EN[${pe}]`);
}

if (errors.length > 0) {
  console.error(`i18n check GAGAL (${errors.length}):`);
  for (const e of errors.slice(0, 40)) console.error(" -", e);
  process.exit(1);
}
console.log(`i18n check OK: ${entries.length} entri (ID→EN), placeholder konsisten.`);
