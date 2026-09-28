#!/usr/bin/env python3
"""Guard paritas lokalisasi receiver (R8b).

Memastikan setiap nama string di `values/strings.xml` punya padanan di
`values-en/strings.xml` (dan sebaliknya), serta format argumen (%1$s, dst.)
konsisten antar bahasa. Dijalankan di CI agar string baru tidak lolos tanpa
terjemahan.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LOCALES = {
    "id": ROOT / "app/src/main/res/values/strings.xml",
    "en": ROOT / "app/src/main/res/values-en/strings.xml",
}
STR_RE = re.compile(r'<string name="([^"]+)"[^>]*>(.*?)</string>', re.S)
ARG_RE = re.compile(r"%\d+\$[a-zA-Z]|%[a-zA-Z]")


def load(path: Path) -> dict[str, str]:
    if not path.exists():
        sys.exit(f"GAGAL: berkas tidak ditemukan: {path}")
    return {m.group(1): m.group(2) for m in STR_RE.finditer(path.read_text("utf-8"))}


def args_of(value: str) -> set[str]:
    return set(ARG_RE.findall(value))


def main() -> int:
    strings = {loc: load(path) for loc, path in LOCALES.items()}
    errors: list[str] = []

    missing_in_en = sorted(set(strings["id"]) - set(strings["en"]))
    extra_in_en = sorted(set(strings["en"]) - set(strings["id"]))
    for name in missing_in_en:
        errors.append(f"tidak ada di values-en: {name}")
    for name in extra_in_en:
        errors.append(f"tidak ada di values (ID): {name}")

    for name in sorted(set(strings["id"]) & set(strings["en"])):
        id_args = args_of(strings["id"][name])
        en_args = args_of(strings["en"][name])
        if id_args != en_args:
            errors.append(
                f"argumen beda utk {name}: ID{sorted(id_args)} vs EN{sorted(en_args)}"
            )

    if errors:
        print(f"i18n receiver GAGAL ({len(errors)}):")
        for e in errors[:40]:
            print(" -", e)
        return 1

    print(f"i18n receiver OK: {len(strings['id'])} string (ID/EN paritas + argumen).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
