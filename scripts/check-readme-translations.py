#!/usr/bin/env python3
"""Keep the localized READMEs in lockstep with README.md.

Every translated README must:
  1. carry a source stamp `<!-- source: README.md sha256:<12-hex> -->`
     matching the current hash of README.md — so any English edit fails CI
     until the translations are refreshed;
  2. contain exactly the same fenced code blocks, in the same order
     (commands are never translated);
  3. contain every non-language-switcher URL the English README contains;
  4. have the same number of `##` sections.

Run: python3 scripts/check-readme-translations.py
"""

from __future__ import annotations

import hashlib
import posixpath
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "README.md"
TRANSLATIONS = [
    "docs/i18n/README.zh-CN.md",
    "docs/i18n/README.ja-JP.md",
    "docs/i18n/README.vi.md",
    "docs/i18n/README.id.md",
    "docs/i18n/README.ko-KR.md",
    "docs/i18n/README.es-419.md",
    "docs/i18n/README.pt-BR.md",
    "docs/i18n/README.ru.md",
    "docs/i18n/README.uk.md",
    "docs/i18n/README.fr.md",
    "docs/i18n/README.de.md",
    "docs/i18n/README.zh-TW.md",
    "docs/i18n/README.hi.md",
    "docs/i18n/README.tr.md",
    "docs/i18n/README.it.md",
    "docs/i18n/README.pl.md",
    "docs/i18n/README.ar.md",
    "docs/i18n/README.ca.md",
]
STAMP_RE = re.compile(r"<!--\s*source:\s*README\.md\s+sha256:([0-9a-f]{12})\s*-->")
FENCE_RE = re.compile(r"```[a-z]*\n(.*?)```", re.DOTALL)
URL_RE = re.compile(r"\((https?://[^)\s]+|(?:docs/|\.\./)[^)\s]+|[A-Za-z0-9_./-]+\.md[^)\s]*)\)")
# The language-switcher line legitimately differs per translation (each file
# links the *other* languages), so its links are exempt from the URL check.
LANGUAGE_LINKS = {
    "README.md",
    "README.zh-CN.md",
    "README.ja-JP.md",
    "README.vi.md",
    "README.id.md",
    "README.ko-KR.md",
    "README.es-419.md",
    "README.pt-BR.md",
    "README.ru.md",
    "README.uk.md",
    "README.fr.md",
    "README.de.md",
    "README.zh-TW.md",
    "README.hi.md",
    "README.tr.md",
    "README.it.md",
    "README.pl.md",
    "README.ar.md",
    "README.ca.md",
}


def source_stamp() -> str:
    return hashlib.sha256(SOURCE.read_bytes()).hexdigest()[:12]


def fences(text: str) -> list[str]:
    return [m.strip() for m in FENCE_RE.findall(text)]


def urls(text: str, document: str = "README.md") -> set[str]:
    """Links that must agree with README.md, independent of where a translation sits.

    Relative links resolve from the document's repository directory. The
    language switcher legitimately differs per file; external URLs are compared
    verbatim.
    """
    out = set()
    for url in URL_RE.findall(text):
        if url in LANGUAGE_LINKS or url.rsplit("/", 1)[-1] in LANGUAGE_LINKS:
            continue
        out.add(
            url if url.startswith(("https://", "http://"))
            else posixpath.normpath(posixpath.join(posixpath.dirname(document), url))
        )
    return out


def sections(text: str) -> int:
    return len(re.findall(r"^## ", text, re.MULTILINE))


def main() -> int:
    expected = source_stamp()
    en = SOURCE.read_text()
    en_fences = fences(en)
    en_urls = urls(en)
    en_sections = sections(en)
    failures: list[str] = []

    for name in TRANSLATIONS:
        path = ROOT / name
        if not path.exists():
            failures.append(f"{name}: missing")
            continue
        text = path.read_text()

        stamp = STAMP_RE.search(text)
        if not stamp:
            failures.append(
                f"{name}: no source stamp — add "
                f"'<!-- source: README.md sha256:{expected} -->'"
            )
        elif stamp.group(1) != expected:
            failures.append(
                f"{name}: stale (stamped {stamp.group(1)}, README.md is now "
                f"{expected}) — retranslate, then update the stamp"
            )

        tr_fences = fences(text)
        if tr_fences != en_fences:
            failures.append(
                f"{name}: code blocks differ from README.md "
                f"({len(tr_fences)} vs {len(en_fences)}; commands must never "
                f"be translated or reordered)"
            )

        missing = en_urls - urls(text, name)
        if missing:
            failures.append(f"{name}: missing links: {sorted(missing)[:5]}")

        if sections(text) != en_sections:
            failures.append(
                f"{name}: {sections(text)} '##' sections vs README.md's "
                f"{en_sections}"
            )

    if failures:
        print("README translation check FAILED:")
        for f in failures:
            print(f"  - {f}")
        print(f"\nCurrent README.md stamp: sha256:{expected}")
        return 1

    print(
        f"README translation check OK — {len(TRANSLATIONS)} translations in "
        f"sync with README.md (sha256:{expected})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
