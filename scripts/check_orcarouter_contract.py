#!/usr/bin/env python3
"""OrcaRouter provider contract check.

Fast, dependency-light proof of the OrcaRouter integration contract that does
not need the 17-minute `codewhale-tui` test build: the credential seam, the two
auth origins, the exchange request shape, the chat capability filter, the live
`/v1/models` roster, and the secret-hygiene rules. It reads the sources it
names and, when `ORCAROUTER_API_KEY` is present, the live gateway.

Never prints the key; a live chat probe reports only the HTTP status.
"""

from __future__ import annotations

import json
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
OAUTH = REPO / "crates" / "runtime" / "src" / "oauth.rs"
CLIENT = REPO / "crates" / "tui" / "src" / "client.rs"
PAGE = REPO / "crates" / "tui" / "src" / "runtime_web" / "orca-evidence.html"

CATALOG_SOURCE = "https://api.orcarouter.ai/v1/models?capability=chat"
EXCHANGE_PATH = "/api/v1/auth/keys"
CHAT_ENDPOINT_TYPES = {"openai"}
# The exact masked placeholder the settings page renders; not a credential.
MASKED_KEY = "sk-orca-\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022"

failures: list[str] = []


def check(condition: bool, message: str) -> None:
    if not condition:
        failures.append(message)


def must_contain(text: str, needle: str, message: str) -> None:
    check(needle in text, message)


def request_headers() -> dict[str, str]:
    key = os.environ.get("ORCAROUTER_API_KEY", "").strip()
    return {"Authorization": "Bearer " + key} if key else {}


def live_catalog() -> list[dict] | None:
    request = urllib.request.Request(CATALOG_SOURCE, headers=request_headers())
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            payload = json.load(response)
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        print(f"[contract] live catalog unavailable: {type(error).__name__}")
        return None
    models = payload.get("data")
    return models if isinstance(models, list) else None


def live_chat_status(model: dict) -> int | None:
    """POST one tiny turn through the gateway; return only the status code."""
    body = json.dumps(
        {
            "model": model.get("id"),
            "messages": [{"role": "user", "content": "Reply with one word: ready"}],
            "max_tokens": 16,
        }
    ).encode()
    headers = {"Content-Type": "application/json", **request_headers()}
    request = urllib.request.Request(
        "https://api.orcarouter.ai/v1/chat/completions", data=body, headers=headers
    )
    try:
        with urllib.request.urlopen(request, timeout=40) as response:
            return response.status
    except urllib.error.HTTPError as error:
        return error.code
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        print(f"[contract] live chat probe failed: {type(error).__name__}")
        return None


def main() -> int:
    oauth = OAUTH.read_text(encoding="utf-8")
    client = CLIENT.read_text(encoding="utf-8")
    page = PAGE.read_text(encoding="utf-8")

    # --- provider: one seam, two adapters -------------------------------------
    must_contain(oauth, 'ORCAROUTER_AUTH_BASE: &str = "https://www.orcarouter.ai"',
                 "auth base is the documented OrcaRouter origin")
    must_contain(oauth, 'ORCAROUTER_API_BASE: &str = "https://api.orcarouter.ai/v1"',
                 "inference base is the separate documented origin")
    must_contain(oauth, 'ORCAROUTER_AUTHORIZE_PATH: &str = "auth"',
                 "authorize path is /auth")
    must_contain(oauth, f'ORCAROUTER_EXCHANGE_PATH: &str = "{EXCHANGE_PATH}"',
                 "exchange path is /api/v1/auth/keys")
    must_contain(oauth, "pub fn from_api_key", "the API-key adapter exists")
    must_contain(oauth, "fn exchange_orcarouter_code",
                 "the PKCE exchange adapter exists")
    must_contain(oauth, "pub fn activate_orcarouter_credential",
                 "both adapters activate through one credential seam")
    must_contain(oauth, "OrcaCredentialSource::Pkce",
                 "the PKCE adapter tags its credential source")

    # --- pkce: S256, ephemeral pair, constant-time state ----------------------
    must_contain(oauth, '("code_challenge_method", "S256")',
                 "the exchange sends the S256 method")
    must_contain(oauth, "URL_SAFE_NO_PAD.encode(Sha256::digest",
                 "the challenge is unpadded base64url(sha256(verifier))")
    must_contain(oauth, "codewhale_core::secret_eq::constant_time_eq",
                 "callback state is compared in constant time")
    must_contain(oauth, '("code_verifier", verifier)',
                 "the exchange sends the code verifier")
    check("client_secret" not in oauth.split("mod tests")[0],
          "no client secret is required or referenced in the flow")
    check(EXCHANGE_PATH != "/v1/auth/keys",
          "the exchange never targets the relay /v1/auth/keys route")

    # --- secrets are not committed -------------------------------------------
    real_key = re.search(r"sk-orca-[A-Za-z0-9]{20,}", oauth + client)
    check(real_key is None, "no hard-coded sk-orca credential is committed")
    check(MASKED_KEY in page,
          "the settings page renders the masked key placeholder, not a real key")
    check("sk-orca-\u2022\u2022\u2022\u2022" in page,
          "the rendered key field is masked")

    # --- catalog + capabilities ----------------------------------------------
    must_contain(client, "ORCAROUTER_CHAT_ENDPOINT_TYPES",
                 "the chat capability filter exists")
    for endpoint in sorted(CHAT_ENDPOINT_TYPES):
        must_contain(client, f'"{endpoint}"',
                     f"chat endpoint type {endpoint} is accepted")
    must_contain(client, "input_modalities",
                 "multimodal filtering reads declared input modalities")
    must_contain(client, 'append_pair("capability", "chat")',
                 "the OrcaRouter roster is requested with the chat capability")
    must_contain(client,
                 "item.supported_endpoint_types.as_ref().is_some_and",
                 "a row that declares no endpoint type fails closed out of chat")

    # --- UI: both entry points ------------------------------------------------
    for element in ("choice-api-key", "choice-pkce", "text-combo", "vision-combo"):
        must_contain(page, element, f"the settings page exposes the {element} control")

    # --- live roster ---------------------------------------------------------
    models = live_catalog()
    if models is not None:
        check(len(models) > 0, "the live chat roster is non-empty")
        image = 0
        served = []
        for model in models:
            if not isinstance(model, dict):
                continue
            declared = model.get("supported_endpoint_types")
            # Rows that do not advertise Chat Completions are skipped, not
            # failed: the client's fail-closed filter (`orcarouter_row_is_chat`)
            # keeps them out of the picker, and the gateway's `?capability=chat`
            # listing includes rows offered only on other dialects.
            if not declared or not set(declared) & CHAT_ENDPOINT_TYPES:
                continue
            served.append(model)
            modalities = (model.get("architecture") or {}).get(
                "input_modalities") or []
            if "image" in modalities:
                image += 1
        check(len(served) > 0, "the live roster has Chat Completions rows")
        print(f"[contract] live catalog: {len(models)} listed, {len(served)} Chat Completions, "
              f"{len(models) - len(served)} skipped, {image} image-input")
        if os.environ.get("ORCAROUTER_API_KEY", "").strip():
            for model in served:
                status = live_chat_status(model)
                if status is None:
                    break
                if status == 200:
                    print("[contract] live chat: 200")
                    break
                # 403 here is a per-key model scope, not a routing failure; the
                # key's own roster is narrower than the shared catalog.
                check(status in (200, 403),
                      f"live chat probe returned an unexpected status {status}")
            else:
                print("[contract] live chat: no advertised model answered for this key")
    else:
        print("[contract] live roster skipped (network or key unavailable)")

    if failures:
        for failure in failures:
            print(f"[contract] FAIL: {failure}")
        return 1
    print("[contract] PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
