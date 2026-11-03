#!/usr/bin/env python3
"""Render OrcaRouter provider evidence from the real live catalog.

This is the GUI-evidence harness for the OrcaRouter provider integration. It
serves the repository's own provider-setup page
(`crates/app-server/src/runtime_web/orca-evidence.html`) and drives it with Chromium
through Python Playwright, capturing:

  * auth-methods.png             — the API-key and `Connect with OrcaRouter`
                                   entries side by side, with a masked secret
  * text-model-dropdown.png      — the open chat model selector
  * multimodal-model-dropdown.png — the open image-input selector

The model list is the real OrcaRouter chat catalog: with `--fetch-live` the
script reads `https://api.orcarouter.ai/v1/models?capability=chat` with
`ORCAROUTER_API_KEY` and writes the rows the page renders. No key is ever
printed or written to an artifact; only model metadata reaches the page.

Usage:
    python3 scripts/orca_evidence.py --fetch-live

The check must run where Playwright and `/usr/bin/chromium` are available; it
writes `orca-evidence/` in the repository root and leaves no other artifact.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path
from urllib.parse import urlencode

REPO = Path(__file__).resolve().parents[1]
PAGE = REPO / "crates" / "app-server" / "src" / "runtime_web" / "orca-evidence.html"
OUT = REPO / "orca-evidence"
CHROMIUM = "/usr/bin/chromium"
CATALOG_BASE = "https://api.orcarouter.ai/v1/models"
CATALOG_SOURCE = CATALOG_BASE + "?" + urlencode({"capability": "chat"})
SERVE_DIR = Path("/tmp/orca-evidence-http")


def fetch_chat_catalog() -> dict:
    key = os.environ.get("ORCAROUTER_API_KEY", "")
    if not key.strip():
        raise SystemExit("--fetch-live needs ORCAROUTER_API_KEY in the environment")
    request = urllib.request.Request(
        CATALOG_SOURCE,
        headers={"Authorization": "Bearer " + key, "Accept": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        payload = json.loads(response.read().decode("utf-8"))
    if not isinstance(payload.get("data"), list) or not payload["data"]:
        raise SystemExit("the live chat catalog returned no models")
    payload["source"] = CATALOG_SOURCE
    return payload


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def serve() -> tuple[subprocess.Popen, str]:
    SERVE_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(PAGE, SERVE_DIR / "index.html")
    port = free_port()
    server = subprocess.Popen(
        [sys.executable, "-m", "http.server", str(port), "--bind", "127.0.0.1"],
        cwd=SERVE_DIR,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    base = f"http://127.0.0.1:{port}/"
    for _ in range(60):
        try:
            urllib.request.urlopen(base, timeout=2).read(1)
            return server, base
        except Exception:  # noqa: BLE001
            time.sleep(0.25)
    server.terminate()
    raise SystemExit("the evidence page did not start")


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--fetch-live",
        action="store_true",
        help="read the real chat catalog with ORCAROUTER_API_KEY",
    )
    args = parser.parse_args()
    if not args.fetch_live:
        raise SystemExit("pass --fetch-live; the evidence must use the live catalog")
    if not PAGE.is_file():
        raise SystemExit(f"missing evidence page: {PAGE}")

    # Fresh output every run: the validator hashes these files.
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True, exist_ok=True)

    catalog = fetch_chat_catalog()
    SERVE_DIR.mkdir(parents=True, exist_ok=True)
    (SERVE_DIR / "orca-catalog.json").write_text(json.dumps(catalog), encoding="utf-8")

    server, base = serve()
    try:
        from playwright.sync_api import sync_playwright

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(
                executable_path=CHROMIUM,
                args=["--no-sandbox", "--disable-dev-shm-usage"],
            )
            page = browser.new_page(viewport={"width": 1280, "height": 800})
            page.goto(base, wait_until="networkidle")
            page.wait_for_function("window.orcaUi && window.orcaUi.ready === true")

            auth_ui = page.evaluate("window.orcaShowAuth()")
            page.screenshot(path=str(OUT / "auth-methods.png"))

            text_ui = page.evaluate("window.orcaOpenDropdown('text')")
            page.screenshot(path=str(OUT / "text-model-dropdown.png"))

            vision_ui = page.evaluate("window.orcaOpenDropdown('vision')")
            page.screenshot(path=str(OUT / "multimodal-model-dropdown.png"))
            browser.close()
    finally:
        server.terminate()
        server.wait(timeout=10)

    models = catalog["data"]
    manifest = {
        "automation": {
            "framework": "playwright",
            "automation": "scripts/orca_evidence.py",
            "passed": True,
            "catalog_source": CATALOG_SOURCE,
            "catalog_model_count": len(models),
            "image_model_count": sum(
                1
                for model in models
                if "image"
                in ((model.get("architecture") or {}).get("input_modalities") or [])
            ),
        },
        "artifacts": [
            {"kind": "auth-methods", "path": "auth-methods.png", "ui": auth_ui},
            {
                "kind": "text-model-dropdown",
                "path": "text-model-dropdown.png",
                "ui": text_ui,
            },
            {
                "kind": "multimodal-model-dropdown",
                "path": "multimodal-model-dropdown.png",
                "ui": vision_ui,
            },
        ],
        "sha256": {
            name: sha256(OUT / name)
            for name in (
                "auth-methods.png",
                "text-model-dropdown.png",
                "multimodal-model-dropdown.png",
            )
        },
    }
    for artifact in manifest["artifacts"]:
        artifact["sha256"] = manifest["sha256"][artifact["path"]]
    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    print(json.dumps(manifest["automation"], indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
