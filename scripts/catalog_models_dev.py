#!/usr/bin/env python3
"""Models.dev catalog refresh / snapshot automation for CodeWhale (#4117).

Fetches the public Models.dev combined catalog, validates offline bundled seed
shape, supports OpenRouter public-listing inspection, and generates the offline
seed (#6396). It never accepts, prints, or persists API keys / auth headers.
The only thing it writes from fetched JSON is the seed lock: the rows the spec
references, projected onto allowlisted fields with credential-shaped keys
scrubbed. The raw document is never written.

Usage examples:

  # Regenerate the offline seed (see docs/CATALOG_REFRESH.md)
  scripts/catalog_models_dev.py seed lock --dry-run   # review report only
  scripts/catalog_models_dev.py seed lock             # pin upstream rows
  scripts/catalog_models_dev.py seed render           # write the seed
  scripts/catalog_models_dev.py seed render --check   # CI: seed == render
  scripts/catalog_models_dev.py seed age --max-days 30 # CI: the pin is not stale
  scripts/catalog_models_dev.py seed discover [--write] # new allowlisted-vendor models

  # Dry-run: fetch + validate, print counts (no write)
  scripts/catalog_models_dev.py refresh

  # Validate the in-repo offline seed still parses as Models.dev-shaped JSON
  scripts/catalog_models_dev.py snapshot --check \\
      crates/config/assets/models_dev.bundled.json

  # OpenRouter public /models listing (no key), dry-run only
  scripts/catalog_models_dev.py refresh --provider openrouter \\
      --sort newest --limit 100

Environment:
  CODEWHALE_MODELS_DEV_URL   Override Models.dev catalog URL
  CODEWHALE_MODELS_DEV_PATH  Read catalog JSON from a local file instead of network
"""

from __future__ import annotations

import argparse
import difflib
import hashlib
import json
import os
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

DEFAULT_MODELS_DEV_URL = "https://models.dev/catalog.json"
DEFAULT_OPENROUTER_MODELS_URL = "https://openrouter.ai/api/v1/models"
USER_AGENT = "CodeWhale-catalog-automation/0.9.0 (+https://github.com/codewhale-hq/CodeWhale)"
FETCH_TIMEOUT_SECS = 60


def die(msg: str, code: int = 1) -> None:
    print(f"error: {msg}", file=sys.stderr)
    raise SystemExit(code)


def load_json_bytes(raw: bytes, source: str) -> Any:
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        die(f"{source}: not utf-8 ({exc})")
    try:
        return json.loads(text)
    except json.JSONDecodeError as exc:
        die(f"{source}: invalid JSON ({exc})")


def fetch_url(url: str) -> bytes:
    req = urllib.request.Request(
        url,
        headers={
            "User-Agent": USER_AGENT,
            "Accept": "application/json",
            # Explicitly no Authorization header — public endpoints only.
        },
        method="GET",
    )
    try:
        with urllib.request.urlopen(req, timeout=FETCH_TIMEOUT_SECS) as resp:
            # Refuse to follow into non-JSON surprise payloads larger than 64 MiB.
            data = resp.read(64 * 1024 * 1024 + 1)
            if len(data) > 64 * 1024 * 1024:
                die(f"{url}: response exceeds 64 MiB safety cap")
            ctype = resp.headers.get("Content-Type", "")
            if "json" not in ctype.lower() and not data.lstrip().startswith((b"{", b"[")):
                die(f"{url}: unexpected Content-Type {ctype!r}")
            return data
    except urllib.error.HTTPError as exc:
        die(f"{url}: HTTP {exc.code} {exc.reason}")
    except urllib.error.URLError as exc:
        die(f"{url}: {exc.reason}")


def load_models_dev_catalog() -> tuple[dict[str, Any], str, bool]:
    """Return (document, source_label, is_local_file).

    Network fetches are dry-run only for write paths: CodeQL treats remote JSON
    as potentially sensitive, and Models.dev is large enough that maintainers
    should stage via CODEWHALE_MODELS_DEV_PATH before writing a cache/snapshot.
    """
    path_override = os.environ.get("CODEWHALE_MODELS_DEV_PATH", "").strip()
    if path_override:
        p = Path(path_override)
        if not p.is_file():
            die(f"CODEWHALE_MODELS_DEV_PATH not a file: {p}")
        raw = p.read_bytes()
        data = load_json_bytes(raw, str(p))
        return ensure_models_dev_shape(data, str(p)), f"file:{p}", True

    url = os.environ.get("CODEWHALE_MODELS_DEV_URL", DEFAULT_MODELS_DEV_URL).strip()
    if not url:
        url = DEFAULT_MODELS_DEV_URL
    raw = fetch_url(url)
    data = load_json_bytes(raw, url)
    return ensure_models_dev_shape(data, url), f"url:{url}", False


def ensure_models_dev_shape(data: Any, source: str) -> dict[str, Any]:
    if not isinstance(data, dict):
        die(f"{source}: expected object root")
    # Allow optional _meta (CodeWhale offline seed) and require models+providers
    # when present so we never write a partial secret leak document.
    models = data.get("models")
    providers = data.get("providers")
    if models is None and providers is None:
        die(f"{source}: missing both 'models' and 'providers'")
    if models is not None and not isinstance(models, dict):
        die(f"{source}: 'models' must be an object")
    if providers is not None and not isinstance(providers, dict):
        die(f"{source}: 'providers' must be an object")
    # Rebuild a public document from allowlisted top-level keys only so we never
    # persist credential-shaped fields even if a future Models.dev field adds them.
    return public_models_dev_document(data)


def is_credential_key(key: str) -> bool:
    banned_exact = {
        "api_key",
        "apikey",
        "authorization",
        "token",
        "access_token",
        "refresh_token",
        "secret",
        "password",
        "client_secret",
    }
    lowered = key.lower()
    return lowered in banned_exact or lowered.endswith("_api_key") or lowered.endswith("_secret")


def strip_sensitive_fields(node: Any) -> Any:
    """Drop keys that look like credentials; never persist auth material."""
    if isinstance(node, dict):
        out: dict[str, Any] = {}
        for key, value in node.items():
            if not isinstance(key, str) or is_credential_key(key):
                continue
            out[key] = strip_sensitive_fields(value)
        return out
    if isinstance(node, list):
        return [strip_sensitive_fields(item) for item in node]
    if isinstance(node, (str, int, float, bool)) or node is None:
        return node
    # Drop non-JSON-scalar oddities rather than serializing them.
    return None


def public_models_dev_document(data: dict[str, Any]) -> dict[str, Any]:
    """Construct a write-safe Models.dev-shaped document (public metadata only)."""
    out: dict[str, Any] = {}
    if isinstance(data.get("_meta"), dict):
        out["_meta"] = strip_sensitive_fields(data["_meta"])
    if isinstance(data.get("models"), dict):
        out["models"] = strip_sensitive_fields(data["models"])
    if isinstance(data.get("_reviewed"), dict):
        out["_reviewed"] = strip_sensitive_fields(data["_reviewed"])
    if isinstance(data.get("providers"), dict):
        out["providers"] = strip_sensitive_fields(data["providers"])
    return out


def public_source_label(source: str) -> str:
    """Log a catalog origin without query/fragment (tokens live there)."""
    if source.startswith("url:"):
        url = source[4:]
        for sep in ("?", "#"):
            url = url.split(sep, 1)[0]
        return f"url:{url}"
    return source


def public_limit_value(value: Any) -> str:
    """Format a catalog limit for logs. Never print credential-shaped strings.

    Remote catalog JSON is tainted for clear-text-logging rules. Only numeric
    limits are meaningful here; anything else (including token-shaped strings)
    is replaced with a constant so the raw value cannot reach stdout.
    """
    if isinstance(value, bool):
        return "redacted"
    if value is None:
        return "null"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        return format(value, ".6g")
    return "redacted"


def catalog_stats(data: dict[str, Any]) -> str:
    models = data.get("models") or {}
    providers = data.get("providers") or {}
    offerings = 0
    if isinstance(providers, dict):
        for prov in providers.values():
            if isinstance(prov, dict):
                models_map = prov.get("models") or {}
                if isinstance(models_map, dict):
                    offerings += len(models_map)
    return (
        f"providers={len(providers) if isinstance(providers, dict) else 0} "
        f"canonical_models={len(models) if isinstance(models, dict) else 0} "
        f"provider_offerings={offerings}"
    )



def cmd_refresh(args: argparse.Namespace) -> None:
    if args.provider and args.provider.lower() == "openrouter":
        refresh_openrouter(args)
        return
    if args.provider:
        die(
            f"unsupported --provider {args.provider!r} "
            "(supported: openrouter, or omit for Models.dev)"
        )

    data, source, _is_local = load_models_dev_catalog()
    print(f"loaded Models.dev catalog from {source}")
    print(catalog_stats(data))
    if args.write_cache or args.write:
        die(
            "disk writes are intentionally unsupported (secret-free by design); "
            "to update the offline seed use `seed lock` then `seed render`"
        )
    print("dry-run complete (no secrets; no disk write)")


def refresh_openrouter(args: argparse.Namespace) -> None:
    url = DEFAULT_OPENROUTER_MODELS_URL
    raw = fetch_url(url)
    data = load_json_bytes(raw, url)
    if not isinstance(data, dict) or "data" not in data:
        die(f"{url}: expected {{ data: [...] }} envelope")
    rows = data["data"]
    if not isinstance(rows, list):
        die(f"{url}: data is not a list")

    # Optional sort / limit for local inspection — never secrets.
    if args.sort == "newest":
        def created_key(row: Any) -> float:
            if not isinstance(row, dict):
                return 0.0
            created = row.get("created")
            try:
                return float(created)
            except (TypeError, ValueError):
                return 0.0

        rows = sorted(rows, key=created_key, reverse=True)
    if args.limit is not None and args.limit > 0:
        rows = rows[: args.limit]

    # Project only public catalog fields — never the raw response object —
    # so credential-shaped keys cannot reach disk even if OpenRouter adds them.
    public_rows: list[dict[str, Any]] = []
    allowed = {
        "id",
        "name",
        "created",
        "description",
        "context_length",
        "architecture",
        "pricing",
        "top_provider",
        "per_request_limits",
        "supported_parameters",
    }
    for row in rows:
        if not isinstance(row, dict):
            continue
        projected: dict[str, Any] = {}
        for key in allowed:
            if key in row and not is_credential_key(key):
                projected[key] = strip_sensitive_fields(row[key])
        if projected.get("id"):
            public_rows.append(projected)
    payload = {
        "_meta": {
            "source": "openrouter.ai/api/v1/models",
            "note": "Public model listing for cache dogfood; not the Models.dev SoT.",
            "count": len(public_rows),
            "sort": args.sort,
            "limit": args.limit,
        },
        "data": public_rows,
    }
    print(f"loaded OpenRouter models: {len(public_rows)} rows (sort={args.sort}, limit={args.limit})")
    if args.write_cache:
        # OpenRouter listing is always network-sourced; avoid disk write of remote JSON.
        die(
            "OpenRouter refresh is dry-run only (no disk write). "
            "Use Models.dev with CODEWHALE_MODELS_DEV_PATH for offline snapshots."
        )
    else:
        print("dry-run complete (OpenRouter writes disabled; use Models.dev local path for caches)")
    _ = payload  # keep payload construction for future offline path


def cmd_snapshot(args: argparse.Namespace) -> None:
    target = Path(args.path)
    if args.check:
        if not target.is_file():
            die(f"--check: missing {target}")
        raw = target.read_bytes()
        data = load_json_bytes(raw, str(target))
        ensure_models_dev_shape(data, str(target))
        print(f"ok: {target} is Models.dev-shaped ({catalog_stats(data)})")
        return

    data, source, _is_local = load_models_dev_catalog()
    print(f"loaded Models.dev catalog from {source}")
    print(catalog_stats(data))
    if args.write or args.force_full:
        die(
            "disk writes are intentionally unsupported for this automation; "
            "the offline seed is generated: use `seed lock` then `seed render`"
        )
    print("dry-run complete (use --check PATH to validate an existing snapshot)")


# ---------------------------------------------------------------------------
# Offline seed generator (#6396)
#
# The offline seed (crates/config/assets/models_dev.bundled.json) is generated,
# never hand-edited:
#
#   spec  scripts/catalog/models_dev_seed.toml       what to carry (reviewed)
#   lock  scripts/catalog/models_dev_seed.lock.json  upstream rows, pinned
#   seed  crates/config/assets/models_dev.bundled.json  = render(spec, lock)
#
# `seed lock` is the only network step and the review step: it fetches
# Models.dev, keeps only the rows the spec references (allowlisted fields,
# credential-shaped keys scrubbed), and prints what changed. `seed render` is
# offline and deterministic; CI runs `seed render --check`.
#
# The spec selects and maps; it never states a value that contradicts
# upstream. Policy (a withheld price, a clamped limit) lives in the runtime
# corrections file, crates/config/assets/catalog_corrections.json, so it
# holds online as well as offline. The one additive exception is a `curated`
# row for a model upstream does not list yet; `seed lock` fails once upstream
# lists it, so a curated row can never shadow an upstream fact.
# ---------------------------------------------------------------------------

SEED_SPEC = Path("scripts/catalog/models_dev_seed.toml")
SEED_LOCK = Path("scripts/catalog/models_dev_seed.lock.json")
SEED_ASSET = Path("crates/config/assets/models_dev.bundled.json")
CORRECTIONS_ASSET = Path("crates/config/assets/catalog_corrections.json")
DESCRIPTORS_ASSET = Path("crates/config/assets/provider_descriptors.json")
SEED_RENDER_COMMAND = "python3 scripts/catalog_models_dev.py seed render"
# Seeded providers whose fallback pin legitimately differs from the descriptor.
# Identification from the provider's own model list is the authority; this map
# is only for a pin that is not that identified id and must stay that way.
SEED_DEFAULT_EXCEPTIONS: dict[str, str] = {}
# Tokens that mark a row as a smaller/faster or non-stable release: newer than
# the default, but not a replacement for it.
VARIANT_TOKENS: frozenset[str] = frozenset(
    {
        "audio",
        "beta",
        "coder",
        "distill",
        "distilled",
        "embed",
        "embedding",
        "experimental",
        "fast",
        "haiku",
        "image",
        "lite",
        "max",
        "micro",
        "mini",
        "nano",
        "preview",
        "reranker",
        "small",
        "tiny",
        "tts",
        "turbo",
        "vision",
    }
)

# Exactly the fields crates/config/src/models_dev.rs reads, in render order.
PROVIDER_MODEL_FIELDS = (
    "id",
    "base_model",
    "canonical_model_id",
    "name",
    "description",
    "type",
    "family",
    "knowledge",
    "release_date",
    "last_updated",
    "default",
    "attachment",
    "reasoning",
    "reasoning_options",
    "interleaved",
    "tool_call",
    "structured_output",
    "temperature",
    "open_weights",
    "modalities",
    "limit",
    "cost",
    "provider",
    "status",
    "experimental",
)
CANONICAL_MODEL_FIELDS = (
    "id",
    "name",
    "description",
    "type",
    "family",
    "knowledge",
    "release_date",
    "last_updated",
    "attachment",
    "reasoning",
    "tool_call",
    "structured_output",
    "temperature",
    "open_weights",
    "modalities",
    "limit",
    "license",
    "links",
    "weights",
    "benchmarks",
)
NESTED_FIELDS = {
    "limit": ("context", "input", "output"),
    "modalities": ("input", "output"),
    "cost": ("input", "output", "cache_read", "cache_write", "reasoning", "input_audio", "output_audio", "tiers", "context_over_200k"),
    "provider": ("npm", "api", "shape", "body", "headers"),
}
# Mapping-only keys a spec model entry may carry. None of them restates an
# upstream fact: `base_model` is the seed's compatibility join; generated
# upstream provider rows may carry `canonical_model_id`.
SPEC_MODEL_KEYS = {"id", "upstream_id", "from", "base_model", "curated"}
SPEC_PROVIDER_KEYS = {"id", "upstream", "name", "api", "npm", "env", "doc", "default", "models"}
SAFE_PUBLIC_TEXT = re.compile(r"^[A-Za-z0-9 ._:/()+\-]{0,64}$")


def project_fields(row: Any, fields: tuple[str, ...]) -> dict[str, Any]:
    """Keep only allowlisted fields, in a fixed order, scrubbed."""
    if not isinstance(row, dict):
        return {}
    out: dict[str, Any] = {}
    for field in fields:
        if field not in row or row[field] is None:
            continue
        value = strip_sensitive_fields(row[field])
        nested = NESTED_FIELDS.get(field)
        if nested is not None:
            if not isinstance(value, dict):
                continue
            value = {key: value[key] for key in nested if value.get(key) is not None}
            if not value:
                continue
        out[field] = value
    return out


def public_value(value: Any) -> str:
    """Format an upstream value for the review report without echoing secrets."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None or isinstance(value, (int, float)):
        return public_limit_value(value)
    if isinstance(value, str):
        if SAFE_PUBLIC_TEXT.match(value) and not value.lower().startswith(("sk-", "bearer")):
            return value
        return "redacted"
    if isinstance(value, list):
        return "[" + ", ".join(public_value(item) for item in value) + "]"
    if isinstance(value, dict):
        return "{" + ", ".join(
            f"{public_value(key)}: {public_value(item)}" for key, item in value.items()
        ) + "}"
    return "redacted"


def flatten(value: Any, prefix: str = "") -> dict[str, Any]:
    if isinstance(value, dict):
        out: dict[str, Any] = {}
        for key, item in value.items():
            out.update(flatten(item, f"{prefix}.{key}" if prefix else str(key)))
        return out
    return {prefix: value}


def load_seed_spec(path: Path) -> dict[str, Any]:
    import tomllib

    if not path.is_file():
        die(f"seed spec missing: {path}")
    try:
        spec = tomllib.loads(path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as exc:
        die(f"{path}: invalid TOML ({exc})")
    return normalize_seed_spec(spec, str(path))


def normalize_seed_spec(spec: dict[str, Any], source: str) -> dict[str, Any]:
    """Validate the spec and expand shorthand model entries."""
    if not isinstance(spec.get("source"), dict) or not spec["source"].get("url"):
        die(f"{source}: [source].url is required")
    meta = spec.get("meta", {})
    if not isinstance(meta, dict) or not all(isinstance(v, str) for v in meta.values()):
        die(f"{source}: [meta] values must be strings")
    curated: dict[tuple[str, str], dict[str, Any]] = {}
    for entry in spec.get("curated", []):
        key = (entry.get("provider", ""), entry.get("id", ""))
        if not all(key) or not str(entry.get("reason", "")).strip():
            die(f"{source}: curated rows need provider, id and a reason")
        if not isinstance(entry.get("row"), dict):
            die(f"{source}: curated {key[0]}/{key[1]} needs a [curated.row] table")
        if key in curated:
            die(f"{source}: curated {key[0]}/{key[1]} is listed twice")
        curated[key] = entry
    providers = []
    seen_providers: set[str] = set()
    for provider in spec.get("providers", []):
        unknown = set(provider) - SPEC_PROVIDER_KEYS
        pid = provider.get("id", "")
        if unknown:
            die(f"{source}: provider {pid} has unknown keys {sorted(unknown)}")
        if not pid or pid in seen_providers:
            die(f"{source}: provider ids must be present and unique ({pid!r})")
        seen_providers.add(pid)
        models = []
        seen_models: set[str] = set()
        for raw in provider.get("models", []):
            entry = {"id": raw} if isinstance(raw, str) else dict(raw)
            unknown = set(entry) - SPEC_MODEL_KEYS
            if unknown:
                die(
                    f"{source}: {pid}/{entry.get('id')} has unknown keys {sorted(unknown)}; "
                    "the spec maps rows, it never restates upstream values "
                    "(policy goes in crates/config/assets/catalog_corrections.json)"
                )
            mid = entry.get("id", "")
            if not mid or mid in seen_models:
                die(f"{source}: {pid} model ids must be present and unique ({mid!r})")
            seen_models.add(mid)
            if entry.get("curated") and (pid, mid) not in curated:
                die(f"{source}: {pid}/{mid} is marked curated but has no [[curated]] row")
            if not entry.get("curated") and (pid, mid) in curated:
                die(f"{source}: {pid}/{mid} has a [[curated]] row but is not marked curated")
            models.append(entry)
        defaults = [m["id"] for m in models if m["id"] == provider.get("default")]
        if len(defaults) != 1:
            die(f"{source}: provider {pid} must name exactly one default among its models")
        providers.append({**provider, "upstream": provider.get("upstream", pid), "models": models})
    used_curated = {
        (p["id"], m["id"]) for p in providers for m in p["models"] if m.get("curated")
    }
    for key in curated:
        if key not in used_curated:
            die(f"{source}: curated {key[0]}/{key[1]} is not listed under its provider")
    canonical = []
    for entry in spec.get("canonical", []):
        entry = {"key": entry} if isinstance(entry, str) else dict(entry)
        if not entry.get("key"):
            die(f"{source}: canonical entries need a key")
        canonical.append({"key": entry["key"], "upstream": entry.get("upstream", entry["key"])})
    discover = spec.get("discover", {})
    skip = discover.get("skip", [])
    since_days = discover.get("since_days", DISCOVER_DEFAULT_SINCE_DAYS)
    if (
        not isinstance(skip, list)
        or not all(isinstance(item, str) for item in skip)
        or not isinstance(since_days, int)
        or isinstance(since_days, bool)
        or since_days <= 0
    ):
        die(f"{source}: [discover] needs skip = [ids] and a positive integer since_days")
    return {
        "source": spec["source"],
        "meta": meta,
        "discover": {"skip": skip, "since_days": since_days},
        "providers": providers,
        "curated": curated,
        "canonical": canonical,
    }


def descriptor_defaults(path: Path) -> dict[str, str]:
    """Default model per provider id, read from the shipped descriptor list."""
    document = read_json_file(path)
    providers = document.get("providers")
    if not isinstance(providers, list):
        die(f"{path}: `providers` must be a list of provider descriptors")
    defaults: dict[str, str] = {}
    for entry in providers:
        if not isinstance(entry, dict) or not isinstance(entry.get("default_model"), str):
            continue
        for key in ("id", "catalog_id"):
            name = entry.get(key)
            if isinstance(name, str) and name:
                defaults.setdefault(name, entry["default_model"])
    return defaults


def version_id(model_id: str) -> tuple[str, int, tuple[int, ...]] | None:
    """`stem-major.minor` only.

    `grok-4.7` and `muse-spark-1.3` match. `gpt-6`, `gpt-6.1-sol`,
    `gemini-3.1-pro-preview`, `kimi-k2.7-code` and dated snapshots do not:
    a different product line, a major jump, or an extra suffix is not a
    successor of the pinned id.
    """
    match = re.fullmatch(r"(.+)-(\d+)\.(\d+(?:\.\d+)*)", model_id.strip())
    if match is None:
        return None
    return (
        match.group(1).lower(),
        int(match.group(2)),
        tuple(int(part) for part in match.group(3).split(".")),
    )


def variant_id(model_id: str, name: str = "") -> bool:
    tokens = [token for token in re.split(r"[^a-z0-9]+", f"{model_id} {name}".lower()) if token]
    return any(token in VARIANT_TOKENS or token.startswith("flash") for token in tokens)


def identify_listed_default(rows: list[dict[str, Any]], pinned: str) -> str:
    """Default id implied by one provider's own model list.

    The pin is the fallback when the list has no same-line successor. A
    successor must share stem and major, carry a higher minor, and must not
    be a variant, a retired row, or a regression of tool calling or a
    published context/output limit. A newer row on a different line
    (`gpt-6` beside `gpt-5.6`, `gemini-4-argon` beside `gemini-3.1-pro-preview`,
    `sakana-namazu` beside `fugu`) does not replace the pin. An explicit
    upstream `default` flag, when the list actually carries one, wins over
    both the pin and a successor.
    """
    usable = [row for row in rows if isinstance(row, dict) and isinstance(row.get("id"), str)]
    flagged = [row["id"] for row in usable if row.get("default") is True and row["id"] != pinned]
    # Our own render stamps `default` from this function. Treat a flag that
    # merely repeats the pin as not an upstream choice. A flag on a different
    # id is an explicit list decision.
    if len(flagged) == 1:
        return flagged[0]
    parsed = version_id(pinned)
    if parsed is None:
        return pinned
    pin_row = next((row for row in usable if row["id"] == pinned), {})
    pin_limit = pin_row.get("limit") if isinstance(pin_row.get("limit"), dict) else {}
    stem, major, _rest = parsed
    best_id: str | None = None
    best_rest: tuple[int, ...] | None = None
    best_date = ""
    for row in usable:
        model_id = row["id"]
        if row.get("status") in ("deprecated", "retired"):
            continue
        if row.get("type") not in (None, "language"):
            continue
        if variant_id(model_id, str(row.get("name") or "")):
            continue
        found = version_id(model_id)
        if found is None or found[0] != stem or found[1] != major:
            continue
        if pin_row.get("tool_call") is True and row.get("tool_call") is False:
            continue
        row_limit = row.get("limit") if isinstance(row.get("limit"), dict) else {}
        if any(
            isinstance(pin_limit.get(field), int)
            and isinstance(row_limit.get(field), int)
            and row_limit[field] < pin_limit[field]
            for field in ("context", "output")
        ):
            continue
        date = row.get("release_date") if isinstance(row.get("release_date"), str) else ""
        if best_rest is None or found[2] > best_rest or (found[2] == best_rest and date > best_date):
            best_id, best_rest, best_date = model_id, found[2], date
    return best_id or pinned


def seed_descriptor_default_errors(spec: dict[str, Any], path: Path) -> list[str]:
    """Descriptor defaults that disagree with the provider model list."""
    defaults = descriptor_defaults(path)
    errors: list[str] = []
    for provider in spec["providers"]:
        pid = provider["id"]
        if pid in SEED_DEFAULT_EXCEPTIONS:
            continue
        identified = identify_listed_default(provider["models"], provider["default"])
        declared = defaults.get(pid)
        if declared is None:
            errors.append(f"{pid}: seeded provider has no default_model in {path}")
        elif declared != identified:
            errors.append(
                f"{pid}: {path} defaults to {declared!r} but the model list "
                f"identifies {identified!r} (pin {provider['default']!r})"
            )
    return errors


def upstream_ref(provider: dict[str, Any], model: dict[str, Any]) -> tuple[str, str]:
    """The (upstream provider, upstream model id) a spec row projects."""
    return (
        model.get("from", provider["upstream"]),
        model.get("upstream_id", model["id"]),
    )


def find_row(rows: dict[str, Any], model_id: str) -> tuple[str, Any] | None:
    """Exact id first, then a unique case-insensitive match (`GLM-5.2` ~ `glm-5.2`)."""
    if model_id in rows:
        return model_id, rows[model_id]
    matches = [key for key in rows if key.lower() == model_id.lower()]
    if len(matches) == 1:
        return matches[0], rows[matches[0]]
    return None


def build_seed_lock(
    spec: dict[str, Any], upstream: dict[str, Any], raw: bytes, fetched_at: str
) -> tuple[dict[str, Any], list[str]]:
    """Project the spec's referenced upstream rows. Returns (lock, errors)."""
    errors: list[str] = []
    up_providers = upstream.get("providers") or {}
    up_models = upstream.get("models") or {}
    lock_providers: dict[str, dict[str, Any]] = {}
    for provider in spec["providers"]:
        for model in provider["models"]:
            up_provider, up_id = upstream_ref(provider, model)
            rows = (up_providers.get(up_provider) or {}).get("models") or {}
            found = find_row(rows, up_id)
            label = f"{provider['id']}/{model['id']}"
            if model.get("curated"):
                if found is not None:
                    errors.append(
                        f"{label}: curated, but upstream {up_provider} now lists "
                        f"{found[0]}; drop the [[curated]] row and derive it"
                    )
                continue
            if found is None:
                errors.append(f"{label}: upstream {up_provider} does not list {up_id}")
                continue
            lock_providers.setdefault(up_provider, {})[found[0]] = project_fields(
                found[1], PROVIDER_MODEL_FIELDS
            )
    lock_models: dict[str, Any] = {}
    for entry in spec["canonical"]:
        row = up_models.get(entry["upstream"])
        if row is None:
            errors.append(f"canonical {entry['key']}: upstream does not list {entry['upstream']}")
            continue
        lock_models[entry["upstream"]] = project_fields(row, CANONICAL_MODEL_FIELDS)
    lock = {
        "source": {
            "url": spec["source"]["url"],
            "fetched_at": fetched_at,
            "sha256": hashlib.sha256(raw).hexdigest(),
        },
        "models": dict(sorted(lock_models.items())),
        "providers": {
            key: dict(sorted(rows.items())) for key, rows in sorted(lock_providers.items())
        },
    }
    return lock, errors


def seed_lock_report(
    spec: dict[str, Any],
    old_lock: dict[str, Any] | None,
    new_lock: dict[str, Any],
    upstream: dict[str, Any],
    corrections: dict[str, Any] | None,
) -> list[str]:
    """Human review lines for the PR body: what a re-lock changes."""
    lines: list[str] = []
    old_rows = flatten((old_lock or {}).get("providers", {}))
    old_rows_by_provider = (old_lock or {}).get("providers", {})
    old_rows.update(flatten({"models": (old_lock or {}).get("models", {})}))
    new_rows = flatten(new_lock.get("providers", {}))
    new_rows.update(flatten({"models": new_lock.get("models", {})}))
    changed = [
        f"  {path}: {public_value(old_rows.get(path))} -> {public_value(new_rows.get(path))}"
        for path in sorted(set(old_rows) | set(new_rows))
        if old_rows.get(path) != new_rows.get(path)
    ]
    lines.append(f"field changes: {len(changed)}")
    lines.extend(changed)

    if corrections:
        stale = stale_corrections(spec, new_lock, corrections)
        lines.append(f"stale corrections (upstream now agrees; delete them): {len(stale)}")
        lines.extend(f"  {line}" for line in stale)

    referenced = {
        upstream_ref(p, m)[0]: set() for p in spec["providers"] for m in p["models"]
    }
    for provider in spec["providers"]:
        for model in provider["models"]:
            up_provider, _ = upstream_ref(provider, model)
            referenced[up_provider].update(new_lock["providers"].get(up_provider, {}))
    freshness: list[str] = []
    for provider in spec["providers"]:
        default = next(m for m in provider["models"] if m["id"] == provider["default"])
        up_provider, up_id = upstream_ref(provider, default)
        rows = new_lock["providers"].get(up_provider, {})
        found = find_row(rows, up_id)
        released = (found[1] if found else {}).get("release_date")
        newer = sorted(
            (
                (row["release_date"], model_id)
                for model_id, row in rows.items()
                if model_id != up_id
                and isinstance(row.get("release_date"), str)
                and isinstance(released, str)
                and row["release_date"] > released
            ),
            reverse=True,
        )[:5]
        detail = f"released {public_value(released)}" if released else "no release_date"
        if newer:
            shown = ", ".join(f"{public_value(mid)} ({public_value(date)})" for date, mid in newer)
            freshness.append(f"  {provider['id']}: default {public_value(provider['default'])} {detail}; newer upstream: {shown}")
        else:
            freshness.append(f"  {provider['id']}: default {public_value(provider['default'])} {detail}")
    lines.append("default freshness (information only; decide whether a newer model should be the default):")
    lines.extend(freshness)

    carried: dict[str, set[str]] = {up_provider: set() for up_provider in old_rows_by_provider}
    for provider in spec["providers"]:
        for model in provider["models"]:
            up_provider, up_id = upstream_ref(provider, model)
            if model.get("curated"):
                continue
            found = find_row(new_lock["providers"].get(up_provider, {}), up_id)
            if found is not None:
                carried.setdefault(up_provider, set()).add(found[0])
    rot = [
        f"  {up_provider}.{model_id}"
        for up_provider, rows in sorted(old_rows_by_provider.items())
        for model_id in sorted(set(rows) - carried.get(up_provider, set()))
    ]
    lines.append(
        "lock rows the spec no longer references (removed by this re-lock, not upstream deletions): "
        f"{len(rot)}"
    )
    lines.extend(rot)

    new_upstream: list[str] = []
    for up_provider, carried in sorted(referenced.items()):
        rows = ((upstream.get("providers") or {}).get(up_provider) or {}).get("models") or {}
        extra = sorted(set(rows) - carried)
        if extra:
            shown = ", ".join(public_value(model) for model in extra[:8])
            more = f" (+{len(extra) - 8} more)" if len(extra) > 8 else ""
            new_upstream.append(f"  {up_provider}: {len(extra)} not carried: {shown}{more}")
    lines.append("upstream models not carried (information only):")
    lines.extend(new_upstream or ["  none"])
    return lines


def stale_corrections(
    spec: dict[str, Any], lock: dict[str, Any], corrections: dict[str, Any]
) -> list[str]:
    """Corrections whose patched value upstream now states itself."""
    rows_by_provider: dict[str, dict[str, Any]] = {}
    for provider in spec["providers"]:
        rows = rows_by_provider.setdefault(provider["id"], {})
        for model in provider["models"]:
            up_provider, up_id = upstream_ref(provider, model)
            found = find_row(lock["providers"].get(up_provider, {}), up_id)
            if found is not None:
                rows[model["id"]] = found[1]
    stale: list[str] = []
    for rule in corrections.get("providers", []):
        rows = rows_by_provider.get(rule.get("provider"), {})
        if rows and not any(row.get("cost") for row in rows.values()):
            stale.append(f"{rule['provider']}: pricing_withheld, but no carried row has a price")
    for fix in corrections.get("models", []):
        row = rows_by_provider.get(fix.get("provider"), {}).get(fix.get("id"))
        if row is None:
            continue
        limit = row.get("limit") or {}
        label = f"{fix['provider']}/{fix['id']}"
        if "max_output" in fix and limit.get("output") == fix["max_output"]:
            stale.append(f"{label}: max_output {fix['max_output']} equals upstream")
        if "context_window" in fix and limit.get("context") == fix["context_window"]:
            stale.append(f"{label}: context_window {fix['context_window']} equals upstream")
        if "pricing_withheld" in fix and not row.get("cost"):
            stale.append(f"{label}: pricing_withheld, but upstream lists no price")
        if "reasoning_options" in fix and row.get("reasoning_options") == fix["reasoning_options"]:
            stale.append(f"{label}: reasoning_options equal upstream")
    return stale


def validate_reviewed(data: Any) -> dict[str, Any]:
    """Refuse malformed authored supplements before the deterministic render."""
    def identifier(value: Any) -> bool:
        return isinstance(value, str) and bool(value) and len(value.encode("utf-8")) <= 512 and value == value.strip() and not any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in value)

    def positive(value: Any) -> bool:
        return value is None or isinstance(value, int) and not isinstance(value, bool) and 0 < value <= 0xffffffff

    if not isinstance(data, dict) or not identifier(data.get("revision")):
        die("reviewed model catalog missing or malformed")
    intrinsic = data.get("intrinsic", {})
    if not isinstance(intrinsic, dict):
        die("reviewed intrinsic facts must be an object")
    for key, row in intrinsic.items():
        if not identifier(key) or not isinstance(row, dict) or not identifier(row.get("source")) or any(not positive(row.get(field)) for field in ("context_window", "max_output", "generation_default")) or not (row.get("reasoning") is None or isinstance(row.get("reasoning"), bool)):
            die("malformed reviewed intrinsic fact")
    public = data.get("public_models", [])
    if not isinstance(public, list):
        die("reviewed public models must be an array")
    seen: set[str] = set()
    for row in public:
        if not isinstance(row, dict) or not identifier(row.get("id")) or row["id"] in seen or row["id"].lower() not in intrinsic:
            die("missing or duplicate reviewed public model")
        seen.add(row["id"])
    for key in ("compatibility_aliases", "completion_rosters", "constants", "groups"):
        if not isinstance(data.get(key, {}), dict):
            die(f"reviewed {key} must be an object")
    for aliases in data.get("compatibility_aliases", {}).values():
        if not isinstance(aliases, dict) or any(not identifier(key) or not identifier(value) for key, value in aliases.items()):
            die("malformed reviewed alias")
    for entries in data.get("completion_rosters", {}).values():
        if not isinstance(entries, list) or any(not identifier(value) for value in entries):
            die("malformed reviewed completion roster")
    for reference in data.get("numeric_refs", {}).values():
        if not isinstance(reference, dict) or reference.get("field") not in ("context_window", "max_output", "generation_default") or reference.get("model") not in intrinsic or not positive(intrinsic[reference["model"]].get(reference["field"])) or intrinsic[reference["model"]].get(reference["field"]) is None:
            die("missing or malformed numeric model contract")
    return data


def render_seed(spec: dict[str, Any], lock: dict[str, Any], reviewed_source: dict[str, Any]) -> str:
    """Render the offline seed from spec + lock. Pure and deterministic."""
    errors: list[str] = []
    providers_out: dict[str, Any] = {}
    row_count = 0
    for provider in spec["providers"]:
        models_out: dict[str, Any] = {}
        listed_facts: list[dict[str, Any]] = []
        for model in provider["models"]:
            key = (provider["id"], model["id"])
            up_provider, up_id = upstream_ref(provider, model)
            locked = find_row(lock["providers"].get(up_provider, {}), up_id)
            if model.get("curated"):
                if locked is not None:
                    errors.append(f"{key[0]}/{key[1]}: curated row also present in the lock")
                    continue
                row = project_fields(spec["curated"][key]["row"], PROVIDER_MODEL_FIELDS)
            elif locked is None:
                errors.append(
                    f"{key[0]}/{key[1]}: not in the lock (run `python3 scripts/catalog_models_dev.py seed lock`)"
                )
                continue
            else:
                row = dict(locked[1])
            row["id"] = model["id"]
            if model.get("base_model"):
                canonical = row.get("canonical_model_id")
                if canonical is not None and canonical != model["base_model"]:
                    errors.append(f"{key[0]}/{key[1]}: seed base_model conflicts with upstream canonical_model_id")
                    continue
                row["base_model"] = model["base_model"]
            listed_facts.append(dict(row))
            row.pop("default", None)
            models_out[model["id"]] = project_fields(row, PROVIDER_MODEL_FIELDS)
            row_count += 1
        identified = identify_listed_default(listed_facts, provider["default"])
        if identified in models_out:
            stamped = dict(models_out[identified])
            stamped["default"] = True
            models_out[identified] = project_fields(stamped, PROVIDER_MODEL_FIELDS)
        header = {"id": provider["id"]}
        for field in ("name", "api", "npm", "doc", "env"):
            if field in provider:
                header[field] = provider[field]
        providers_out[provider["id"]] = {**header, "models": models_out}
    models_out = {}
    for entry in spec["canonical"]:
        row = lock["models"].get(entry["upstream"])
        if row is None:
            errors.append(f"canonical {entry['key']}: not in the lock")
            continue
        models_out[entry["key"]] = project_fields({**row, "id": entry["key"]}, CANONICAL_MODEL_FIELDS)
    if errors:
        die("seed render refused:\n  " + "\n  ".join(errors))
    meta = dict(spec["meta"])
    meta["generated_by"] = (
        f"{SEED_RENDER_COMMAND} from {SEED_SPEC} and {SEED_LOCK}. "
        "Do not edit this file by hand; see docs/CATALOG_REFRESH.md."
    )
    source = lock["source"]
    meta["upstream"] = f"{source['url']} fetched {source['fetched_at']} sha256 {source['sha256']}"
    meta["coverage"] = (
        f"{len(providers_out)} providers, {row_count} provider model rows, "
        f"{len(models_out)} canonical model entries."
    )
    reviewed_source = validate_reviewed(reviewed_source)
    document = {"_meta": meta, "models": models_out, "providers": providers_out, "_reviewed": reviewed_source}
    ensure_models_dev_shape(document, "rendered seed")
    return json.dumps(document, indent=2, ensure_ascii=False) + "\n"


def read_json_file(path: Path) -> Any:
    if not path.is_file():
        die(f"missing {path}")
    return load_json_bytes(path.read_bytes(), str(path))


def load_upstream_payload(
    spec: dict[str, Any], snapshot: str | None = None
) -> tuple[bytes, str, dict[str, Any]]:
    """The pinned upstream document: snapshot, local file, then the spec URL."""
    if snapshot:
        raw = Path(snapshot).read_bytes()
        source = f"file:{snapshot}"
    else:
        path_override = os.environ.get("CODEWHALE_MODELS_DEV_PATH", "").strip()
        if path_override:
            raw = Path(path_override).read_bytes()
            source = f"file:{path_override}"
        else:
            url = os.environ.get("CODEWHALE_MODELS_DEV_URL", "").strip() or spec["source"]["url"]
            raw = fetch_url(url)
            source = f"url:{url}"
    return raw, source, ensure_models_dev_shape(load_json_bytes(raw, source), source)


def cmd_defaults_audit(args: argparse.Namespace) -> None:
    spec = load_seed_spec(Path(args.spec))
    lock = read_json_file(Path(getattr(args, "lock", None) or SEED_LOCK))
    defaults = descriptor_defaults(Path(args.descriptors))
    print(
        "rule: a provider default is the same-line successor in that provider's "
        "own model list, or the pin when the list has none. A newer row on a "
        "different line is not a successor. The descriptor must match."
    )
    stale: list[str] = []
    promoted: list[str] = []
    for provider in spec["providers"]:
        rows: list[dict[str, Any]] = []
        for model in provider["models"]:
            up_provider, up_id = upstream_ref(provider, model)
            locked = find_row(lock.get("providers", {}).get(up_provider, {}), up_id)
            row = dict(locked[1]) if locked is not None else {"id": model["id"]}
            row["id"] = model["id"]
            rows.append(row)
        identified = identify_listed_default(rows, provider["default"])
        declared = defaults.get(provider["id"])
        if declared != identified:
            stale.append(
                f"{provider['id']}: descriptor {declared!r}, list identifies {identified!r}, "
                f"pin {provider['default']!r}"
            )
        elif identified != provider["default"]:
            promoted.append(f"{provider['id']}: pin {provider['default']!r} -> {identified}")
    for line in promoted:
        print(f"promoted  {line}")
    for line in stale:
        print(f"STALE     {line}")
    print(f"{len(spec['providers']) - len(stale)} descriptors match their model list; {len(stale)} stale")
    if stale:
        die(
            "provider defaults disagree with their model lists:\n  "
            + "\n  ".join(stale)
            + "\n  set each descriptor default_model to the identified id"
        )


def cmd_seed_lock(args: argparse.Namespace) -> None:
    spec = load_seed_spec(Path(args.spec))
    drift = seed_descriptor_default_errors(spec, Path(args.descriptors))
    if drift:
        die("descriptor/seed default drift:\n  " + "\n  ".join(drift))
    raw, source, upstream = load_upstream_payload(spec)
    fetched_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    new_lock, errors = build_seed_lock(spec, upstream, raw, fetched_at)
    lock_path = Path(args.lock)
    old_lock = read_json_file(lock_path) if lock_path.is_file() else None
    corrections_path = Path(args.corrections)
    corrections = read_json_file(corrections_path) if corrections_path.is_file() else None
    print(f"upstream: {public_source_label(source)} sha256 {new_lock['source']['sha256']}")
    for line in seed_lock_report(spec, old_lock, new_lock, upstream, corrections):
        print(line)
    if errors:
        die("seed lock refused:\n  " + "\n  ".join(errors))
    if args.dry_run:
        print("dry-run: lock not written")
        return
    lock_path.write_text(json.dumps(new_lock, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {lock_path}; next: {SEED_RENDER_COMMAND}")


def cmd_seed_render(args: argparse.Namespace) -> None:
    spec = load_seed_spec(Path(args.spec))
    drift = seed_descriptor_default_errors(spec, Path(args.descriptors))
    if drift:
        die("descriptor/seed default drift:\n  " + "\n  ".join(drift))
    lock = read_json_file(Path(args.lock))
    corrections = read_json_file(Path(args.corrections))
    providers = {provider["id"] for provider in spec["providers"]}
    models = {(provider["id"], model["id"]) for provider in spec["providers"] for model in provider["models"]}
    missing = [str(rule.get("provider")) for rule in corrections.get("providers", []) if rule.get("provider") not in providers]
    missing.extend(f"{row.get('provider')}/{row.get('id')}" for row in corrections.get("models", []) if (row.get("provider"), row.get("id")) not in models)
    if missing:
        die("seed corrections name absent targets: " + ", ".join(missing))
    rendered = render_seed(spec, lock, corrections.get("reviewed"))
    target = Path(args.out)
    if args.check:
        current = target.read_text(encoding="utf-8") if target.is_file() else ""
        if current == rendered:
            print(f"ok: {target} matches {SEED_SPEC} + {SEED_LOCK}")
            return
        diff = list(
            difflib.unified_diff(
                current.splitlines(),
                rendered.splitlines(),
                fromfile=f"{target} (committed)",
                tofile=f"{target} (rendered)",
                lineterm="",
            )
        )
        for line in diff[:200]:
            print(line)
        if len(diff) > 200:
            print(f"... {len(diff) - 200} more diff lines")
        die(
            f"{target} is not the rendered seed. It is generated: put the change in "
            f"{SEED_SPEC} (selection/mapping) or {CORRECTIONS_ASSET} (policy), then run "
            f"`{SEED_RENDER_COMMAND}`"
        )
    target.write_text(rendered, encoding="utf-8")
    print(f"wrote {target}")


def cmd_seed_age(args: argparse.Namespace) -> None:
    lock = read_json_file(Path(args.lock))
    fetched_at = (lock.get("source") or {}).get("fetched_at")
    if not isinstance(fetched_at, str):
        die(f"{args.lock} has no source.fetched_at; the seed was never pinned")
    try:
        pinned = datetime.strptime(fetched_at, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    except ValueError:
        die(f"{args.lock} source.fetched_at is not UTC ISO-8601: {fetched_at!r}")
    age_days = max(0.0, (datetime.now(timezone.utc) - pinned).total_seconds() / 86400.0)
    if age_days > args.max_days:
        die(
            f"the offline seed is {age_days:.1f} days old (limit {args.max_days}d), pinned {fetched_at}.\n"
            "The bundled roster freezes at the last hand-run re-lock, so every install offline "
            "or past the live TTL keeps an old model list. Re-lock and land it:\n"
            "  python3 scripts/catalog_models_dev.py seed lock\n"
            f"  {SEED_RENDER_COMMAND}\n"
            "See docs/CATALOG_REFRESH.md."
        )
    print(f"ok: offline seed pinned {fetched_at} ({age_days:.1f} days old, limit {args.max_days}d)")


DISCOVER_VENDORS: dict[str, tuple[str, str | None, tuple[str, ...]]] = {
    "anthropic": ("Anthropic", "anthropic", ()),
    "openai": ("OpenAI", "openai", ()),
    "google": ("Google", "google", ()),
    "deepseek": ("DeepSeek", "deepseek", ()),
    "xai": ("xAI", "xai", ()),
    "moonshotai": ("Moonshot/Kimi", "moonshot", ()),
    "zai": ("Z.ai", "zai", ()),
    "minimax": ("MiniMax", "minimax", ()),
    "alibaba": ("Qwen", None, ("qwen", "qwq")),
    "mistral": ("Mistral", None, ("mistral", "codestral", "ministral", "devstral", "magistral", "pixtral")),
    "meta": ("Meta", "meta", ()),
    "xiaomi": ("Xiaomi MiMo", "xiaomi-mimo", ()),
    "stepfun": ("StepFun", "stepfun", ()),
}
DISCOVER_DEFAULT_SINCE_DAYS = 45
DISCOVER_NON_CHAT_TOKENS = {
    "asr", "audio", "computer", "embed", "embedding", "image", "imagine", "latest",
    "live", "moderation", "ocr", "realtime", "rerank", "reranker", "research",
    "transcribe", "tts", "video",
}
DISCOVER_SNAPSHOT_ID = re.compile(r"-(\d{8}|\d{4}-\d{2}-\d{2})$")
ISO_DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
DISCOVER_SAFE_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$")


def discover_exclusion(model_id: str, row: dict[str, Any], since: str) -> str | None:
    """Why a models.dev row is not a public chat model, or None when it qualifies."""
    if not DISCOVER_SAFE_ID.match(model_id):
        return "id outside the safe character set"
    release = row.get("release_date")
    if not isinstance(release, str) or not ISO_DATE.match(release):
        return "no release_date"
    if release < since:
        return "older than the window"
    if row.get("status") in ("deprecated", "retired"):
        return "deprecated"
    if DISCOVER_SNAPSHOT_ID.search(model_id):
        return "dated snapshot"
    if DISCOVER_NON_CHAT_TOKENS & set(re.split(r"[^a-z0-9]+", model_id.lower())):
        return "not a chat model id"
    if (row.get("modalities") or {}).get("output") != ["text"] or row.get("tool_call") is not True:
        return "not a text model with tool calling"
    context = (row.get("limit") or {}).get("context")
    if not isinstance(context, int) or isinstance(context, bool) or context <= 0:
        return "no context limit"
    return None


def discover_candidates(
    upstream: dict[str, Any],
    reviewed: dict[str, Any],
    skip: list[str],
    since: str,
) -> list[dict[str, Any]]:
    """Allowlisted-vendor chat models released since `since` that the public list lacks."""
    known = {str(key).lower() for key in reviewed.get("intrinsic", {})}
    for entry in reviewed.get("public_models", []):
        known.add(entry["id"].lower())
        known.update(str(alias).lower() for alias in entry.get("aliases", []))
    known.update(str(item).lower() for item in skip)
    found: list[dict[str, Any]] = []
    for vendor, (label, spec_provider, prefixes) in DISCOVER_VENDORS.items():
        rows = (upstream.get("providers", {}).get(vendor) or {}).get("models") or {}
        for model_id, row in sorted(rows.items()):
            if prefixes and not model_id.lower().startswith(prefixes):
                continue
            if model_id.lower() in known or discover_exclusion(model_id, row, since):
                continue
            limit = row["limit"]
            output = limit.get("output")
            found.append(
                {
                    "vendor": vendor,
                    "label": label,
                    "spec_provider": spec_provider,
                    "id": model_id,
                    "release_date": row["release_date"],
                    "context_window": limit["context"],
                    "max_output": output if isinstance(output, int) and not isinstance(output, bool) and output > 0 else None,
                    "reasoning": row.get("reasoning") is True,
                }
            )
    found.sort(key=lambda item: (item["release_date"], item["vendor"], item["id"]))
    return found


def append_spec_model(spec_text: str, provider_id: str, model_id: str) -> str:
    """Add a bare model id to the end of one provider's `models` array."""
    block = re.search(rf'^\[\[providers\]\]\nid = "{re.escape(provider_id)}"\n', spec_text, re.M)
    if block is None:
        die(f"seed spec has no provider {provider_id}")
    opener = re.compile(r"^models = \[\n", re.M).search(spec_text, block.end())
    nxt = spec_text.find("[[providers]]", block.end())
    if opener is None or (nxt != -1 and opener.start() > nxt):
        die(f"seed spec provider {provider_id}: could not find its models array")
    end = spec_text.index("\n]\n", opener.end() - 1) + 1
    return spec_text[:end] + f'  "{model_id}",\n' + spec_text[end:]


VENDOR_FAMILY = {
    "anthropic": "Anthropic",
    "openai": "OpenAi",
    "google": "Google",
    "deepseek": "DeepSeek",
    "xai": "Xai",
    "moonshotai": "Moonshot",
    "zai": "Zai",
    "minimax": "Minimax",
    "alibaba": "Qwen",
    "mistral": "Mistral",
    "meta": "Meta",
    "xiaomi": "XiaomiMimo",
    "stepfun": "Stepfun",
}


def apply_discoveries(
    found: list[dict[str, Any]],
    corrections: dict[str, Any],
    spec: dict[str, Any],
    spec_text: str,
    today: str,
) -> tuple[dict[str, Any], str]:
    reviewed = corrections["reviewed"]
    seeded = {(p["id"], m["id"].lower()) for p in spec["providers"] for m in p["models"]}
    for item in found:
        intrinsic: dict[str, Any] = {
            "source": f"models.dev {item['vendor']}/{item['id']}, discovered {today}; unreviewed",
            "context_window": item["context_window"],
        }
        if item["max_output"]:
            intrinsic["max_output"] = item["max_output"]
        intrinsic["reasoning"] = item["reasoning"]
        intrinsic["family"] = VENDOR_FAMILY[item["vendor"]]
        reviewed["intrinsic"][item["id"].lower()] = intrinsic
        reviewed["public_models"].append(
            {
                "id": item["id"],
                "label": item["label"],
                "added_at": item["release_date"],
                "aliases": [item["id"]],
            }
        )
        if item["spec_provider"] and (item["spec_provider"], item["id"].lower()) not in seeded:
            spec_text = append_spec_model(spec_text, item["spec_provider"], item["id"])
    return corrections, spec_text


def cmd_seed_discover(args: argparse.Namespace) -> None:
    spec = load_seed_spec(Path(args.spec))
    since = args.since or (
        datetime.now(timezone.utc) - timedelta(days=spec["discover"]["since_days"])
    ).strftime("%Y-%m-%d")
    if not ISO_DATE.match(since):
        die(f"--since must be YYYY-MM-DD, got {since!r}")
    _, source, upstream = load_upstream_payload(spec, args.snapshot)
    corrections_path = Path(args.corrections)
    corrections = read_json_file(corrections_path)
    found = discover_candidates(upstream, corrections["reviewed"], spec["discover"]["skip"], since)
    print(f"upstream: {public_source_label(source)}; vendors: {', '.join(DISCOVER_VENDORS)}; released since {since}")
    for item in found:
        target = item["spec_provider"] or "public list only"
        print(f"  {item['release_date']} {item['vendor']}/{item['id']} ctx {item['context_window']} out {item['max_output']} -> {target}")
    print(f"{len(found)} new model(s)")
    if not args.write or not found:
        return
    spec_path = Path(args.spec)
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    corrections, spec_text = apply_discoveries(
        found, corrections, spec, spec_path.read_text(encoding="utf-8"), today
    )
    corrections_path.write_text(json.dumps(corrections, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    spec_path.write_text(spec_text, encoding="utf-8")
    print(f"wrote {corrections_path} and {spec_path}; next: seed lock, seed render")


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description="Secret-free Models.dev / OpenRouter catalog automation (#4117)"
    )
    sub = p.add_subparsers(dest="cmd", required=True)

    refresh = sub.add_parser("refresh", help="Fetch live catalog / provider models")
    refresh.add_argument(
        "--provider",
        default=None,
        help="Optional provider id (currently: openrouter). Omit for Models.dev.",
    )
    refresh.add_argument(
        "--sort",
        default="newest",
        choices=["newest", "none"],
        help="OpenRouter sort order (default: newest)",
    )
    refresh.add_argument(
        "--limit",
        type=int,
        default=100,
        help="OpenRouter row cap (default: 100; 0 = no cap)",
    )
    refresh.add_argument(
        "--write-cache",
        metavar="PATH",
        help="Deprecated/unsupported: validate-only automation never writes fetched JSON",
    )
    refresh.add_argument(
        "--write",
        metavar="PATH",
        help="Deprecated/unsupported alias of --write-cache",
    )
    refresh.set_defaults(func=cmd_refresh)

    snapshot = sub.add_parser(
        "snapshot",
        help="Validate or write a Models.dev-shaped snapshot document",
    )
    snapshot.add_argument(
        "path",
        nargs="?",
        default="crates/config/assets/models_dev.bundled.json",
        help="Snapshot path (default: offline seed asset)",
    )
    snapshot.add_argument(
        "--check",
        action="store_true",
        help="Validate existing file only (no network)",
    )
    snapshot.add_argument(
        "--write",
        action="store_true",
        help="Deprecated/unsupported: validate-only automation never writes snapshots",
    )
    snapshot.add_argument(
        "--force-full",
        action="store_true",
        help="Deprecated/unsupported with --write; retained for clear failure messages",
    )
    snapshot.set_defaults(func=cmd_snapshot)

    seed = sub.add_parser(
        "seed",
        help="Generate the offline seed from the reviewed spec and a pinned lock",
    )
    seed_sub = seed.add_subparsers(dest="seed_cmd", required=True)
    lock = seed_sub.add_parser(
        "lock",
        help="Fetch upstream, pin the rows the spec references, print the review report",
    )
    lock.add_argument("--dry-run", action="store_true", help="Print the report; write nothing")
    render = seed_sub.add_parser(
        "render", help="Render the seed from spec + lock (offline, deterministic)"
    )
    render.add_argument(
        "--check",
        action="store_true",
        help="Fail with a diff when the committed seed differs from the rendered one",
    )
    render.add_argument("--out", default=str(SEED_ASSET), help="Seed path to write or check")
    age = seed_sub.add_parser(
        "age",
        help="Fail when the pinned seed is older than the allowed age (offline)",
    )
    age.add_argument(
        "--max-days",
        type=float,
        default=30.0,
        help="Maximum age of source.fetched_at in days (default: 30)",
    )
    age.set_defaults(func=cmd_seed_age)
    for command in (lock, render, age):
        command.add_argument("--spec", default=str(SEED_SPEC))
        command.add_argument("--lock", default=str(SEED_LOCK))
        command.add_argument("--corrections", default=str(CORRECTIONS_ASSET))
        command.add_argument("--descriptors", default=str(DESCRIPTORS_ASSET))
    lock.set_defaults(func=cmd_seed_lock)
    render.set_defaults(func=cmd_seed_render)

    discover = seed_sub.add_parser(
        "discover",
        help="List (or with --write, add) allowlisted-vendor models the public list lacks",
    )
    discover.add_argument("--write", action="store_true", help="Add the rows to the spec and corrections")
    discover.add_argument("--since", help="Earliest release_date, YYYY-MM-DD (default: spec window)")
    discover.add_argument("--snapshot", help="Read a local models.dev JSON instead of fetching")
    discover.add_argument("--spec", default=str(SEED_SPEC))
    discover.add_argument("--corrections", default=str(CORRECTIONS_ASSET))
    discover.set_defaults(func=cmd_seed_discover)

    defaults = sub.add_parser(
        "defaults", help="Audit shipped provider defaults against a live catalog"
    )
    defaults_sub = defaults.add_subparsers(dest="defaults_cmd", required=True)
    audit = defaults_sub.add_parser(
        "audit",
        help="Fail when a descriptor default disagrees with its provider model list",
    )
    audit.add_argument("--spec", default=str(SEED_SPEC))
    audit.add_argument("--lock", default=str(SEED_LOCK))
    audit.add_argument("--descriptors", default=str(DESCRIPTORS_ASSET))
    audit.set_defaults(func=cmd_defaults_audit)
    return p


def main(argv: list[str] | None = None) -> None:
    parser = build_parser()
    args = parser.parse_args(argv)
    args.func(args)


if __name__ == "__main__":
    main()
