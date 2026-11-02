#!/usr/bin/env python3
"""Generate the built-in catalog from the first-party marketplace checkout.

No install, trust, enablement, network fetch, or plugin execution occurs here.
Run with --check in CI; without it, review the generated snapshot diff.
"""
import argparse
import json
import re
from pathlib import Path
import subprocess
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "https://github.com/codewhale-hq/codewhale-plugin-marketplace"
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--marketplace", type=Path, default=ROOT.parent / "codewhale-plugin-marketplace")
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
source = args.marketplace.resolve()
raw = subprocess.check_output(["git", "show", "HEAD:marketplace.json"], cwd=source)
revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip()
catalog = json.loads(raw)
for candidate in catalog["plugins"]:
    spec = candidate["source"]
    if not spec.startswith("path:"):
        if re.fullmatch(r"github:[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", spec):
            continue
        parsed = urlsplit(spec)
        if (parsed.scheme == "https" and parsed.hostname and not parsed.username
                and not parsed.password and not any(c.isspace() or ord(c) < 32 for c in spec)):
            continue
        raise SystemExit(f"unexpected first-party source: {spec}")
    relative = spec[5:]
    if any(part in ("", ".", "..") or not all(c.isascii() and (c.isalnum() or c in "-_.") for c in part) for part in relative.split("/")):
        raise SystemExit(f"unsafe bundle path: {relative}")
    # Pin local bundles to the reviewed marketplace revision so the
    # bytes a user installs are the bytes this snapshot describes. Freshness
    # comes from bumping the pin (the marketplace-sync workflow reports drift
    # against `main` weekly); `/plugin update` re-downloads the same archive
    # and reports no change until the pin moves.
    candidate["source"] = f"https://codeload.github.com/codewhale-hq/codewhale-plugin-marketplace/tar.gz/{revision}#path={relative}"
snapshot = {"repository": REPOSITORY, "revision": revision, "catalog": catalog}
rendered = json.dumps(snapshot, indent=2, ensure_ascii=False) + "\n"
output = ROOT / "crates/plugins/assets/first-party-marketplace.json"
if args.check:
    if not output.exists() or output.read_text() != rendered:
        raise SystemExit("First-party catalog drift: run python3 scripts/sync-marketplace.py, review, and rebuild.")
    print(f"First-party catalog matches marketplace {revision}")
else:
    output.write_text(rendered)
    print(f"Updated {output} from marketplace {revision}")
