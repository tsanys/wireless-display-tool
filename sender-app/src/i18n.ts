/**
 * Lokalisasi sender (ID/EN) — R8.
 *
 * Desain minim-risiko:
 * - Teks statis di `index.html` sudah berbahasa Indonesia; `applyLocale()`
 *   menelusuri text-node dan menukar persis sesuai kamus (ID⇄EN), jadi tidak
 *   perlu mengubah struktur markup.
 * - Teks dinamis (status, error, label) memakai `t()`.
 * - Default mengikuti bahasa sistem (`navigator.language`), fallback `id`,
 *   dan pilihan user dipersist di localStorage.
 */

import dict from "./i18n.id-en.json";

export type Locale = "id" | "en";

const STORAGE_KEY = "wdt:locale";
const ID_TO_EN: Record<string, string> = dict;

/** Kebalikan EN → ID (untuk kembali ke Indonesia). */
const EN_TO_ID: Record<string, string> = Object.fromEntries(
  Object.entries(ID_TO_EN).map(([id, en]) => [en, id]),
);

let current: Locale = "id";

function detectDefault(): Locale {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved === "id" || saved === "en") return saved;
  const nav = (navigator.language || "id").toLowerCase();
  return nav.startsWith("id") ? "id" : "en";
}

/** Terjemahkan satu string (dynamic). Default ID = identitas. */
export function t(id: string): string {
  if (current === "id") return id;
  return ID_TO_EN[id] ?? id;
}

/**
 * Terjemahkan template dengan placeholder `{nama}` lalu isi parameternya.
 * Contoh: `tf("{tv} menerima tampilan dari laptop ini.", { tv })`.
 */
export function tf(template: string, params: Record<string, string | number>): string {
  const translated = t(template);
  return translated.replace(/\{(\w+)\}/g, (_, key: string) =>
    key in params ? String(params[key]) : `{${key}}`,
  );
}

export function locale(): Locale {
  return current;
}

/** Simpan + terapkan locale (menukar seluruh teks statis di DOM). */
export function setLocale(next: Locale): void {
  // Pilih kamus berdasarkan TARGET (bukan `current`) agar idempoten:
  // menerapkan locale yang sama hanya mencocokkan teks yang sudah benar
  // (tidak ada yang berubah), dan transisi selalu benar.
  const dict = next === "en" ? ID_TO_EN : EN_TO_ID;
  current = next;
  localStorage.setItem(STORAGE_KEY, next);
  swapTextNodes(document.body, dict);
}

/** Inisialisasi dari preferensi/sistem + pasang pilihan awal. */
export function initLocale(next?: Locale): Locale {
  current = "id";
  const target = next ?? detectDefault();
  if (target === "en") setLocale("en");
  else {
    current = "id";
    localStorage.setItem(STORAGE_KEY, "id");
  }
  return current;
}

/**
 * Tukar text-node yang persis cocok dengan kamus.
 * Tidak menyentuh node dengan nilai dinamis (yang akan di-render ulang JS).
 */
function swapTextNodes(root: Node, dict: Record<string, string>): void {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const hits: Text[] = [];
  let node = walker.nextNode();
  while (node) {
    const text = node as Text;
    const trimmed = text.nodeValue?.trim() ?? "";
    if (trimmed && dict[trimmed]) hits.push(text);
    node = walker.nextNode();
  }
  for (const textNode of hits) {
    const raw = textNode.nodeValue ?? "";
    const trimmed = raw.trim();
    const translated = dict[trimmed];
    if (!translated) continue;
    // Pertahankan whitespace di sekitar teks.
    textNode.nodeValue = raw.replace(trimmed, translated);
  }
}
