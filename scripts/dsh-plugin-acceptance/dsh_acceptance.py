#!/usr/bin/env python3
"""Pinned DSH plugin acceptance driver and receipt validator (CW-7051).

run       drive one pinned row through lifecycle L against an installed Engine
validate  check a receipt against the pins; the verdict is recomputed here
matrix    combine validated receipts into the row x OS table
selftest  prove the validator rejects a missing step and never infers PASS

The driver only uses the Engine's own /v1/apps/plugins routes. It never loads
plugin code itself and never calls a model provider unless --allow-provider is
given. Verdicts: PASS, FAIL, UNAVAILABLE (a named platform, provider, desktop
or artifact is missing), BLOCKED (admission refused as pinned), INVALID.
"""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import platform
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
PINS_PATH = HERE.parents[1] / "crates/engine/tests/fixtures/dsh-plugin-acceptance/pins.json"
RECEIPT_SCHEMA = "codewhale.dsh-acceptance-receipt/1"
MATRIX_SCHEMA = "codewhale.dsh-acceptance-matrix/1"
OSES = ["macos", "linux", "windows"]
STATUSES = {"PASS", "FAIL", "UNAVAILABLE", "SKIPPED"}
UNAVAILABLE_KINDS = {"platform", "provider", "desktop", "artifact"}
LEVELS = {"source", "local", "installed", "provider", "desktop"}

STEP_LEVEL = {
    "pin.tarball_hash": "source",
    "package.extract": "local",
    "engine.identity": "installed",
    "platform.identity": "installed",
    "admission.preview": "installed",
    "admission.refused": "installed",
    "lifecycle.install": "installed",
    "lifecycle.trust": "installed",
    "lifecycle.enable": "installed",
    "lifecycle.restart_retained": "installed",
    "journey.core": "provider",
    "journey.controls": "provider",
    "client.seats_rendered": "desktop",
    "lifecycle.revoke": "installed",
    "lifecycle.uninstall": "installed",
    "security.no_key_leak": "installed",
}
COMMON = ["pin.tarball_hash", "package.extract", "engine.identity", "platform.identity"]
ADMITTED_ROW = [
    "admission.preview", "lifecycle.install", "lifecycle.trust", "lifecycle.enable",
    "journey.core", "journey.controls", "lifecycle.restart_retained",
    "lifecycle.revoke", "lifecycle.uninstall", "security.no_key_leak",
]
REFUSED_ROW = ["admission.refused"]


def os_name() -> str:
    return {"darwin": "macos", "linux": "linux", "win32": "windows"}.get(sys.platform, sys.platform)


def load_pins(path: Path = PINS_PATH) -> dict:
    pins = json.loads(path.read_text())
    if pins.get("schema") != "codewhale.dsh-acceptance-pins/1" or len(pins["rows"]) != 33:
        raise SystemExit("pins file is not the ratified 33-row table")
    return pins


def pin_for(pins: dict, row: int) -> dict:
    for entry in pins["rows"]:
        if entry["row"] == row:
            return entry
    raise SystemExit(f"row {row} is not in the pinned list")


def required_steps(pin: dict) -> list[str]:
    if pin["admission"] != "admit":
        return COMMON + REFUSED_ROW
    steps = COMMON + ADMITTED_ROW
    if pin["declares_client"]:
        steps.insert(steps.index("lifecycle.restart_retained") + 1, "client.seats_rendered")
    return steps


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_json(value) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def refusal_needle(pin: dict) -> str:
    refusal = pin["refusal"]
    return f"{refusal['code']}: `{refusal['field']}` requires DSH `{refusal['range']}`"


def verdict_of(pin: dict, steps: list[dict]) -> str:
    by_id = {step["id"]: step for step in steps}
    statuses = [by_id[name]["status"] for name in required_steps(pin)]
    if "FAIL" in statuses:
        return "FAIL"
    if all(status == "PASS" for status in statuses):
        return "PASS" if pin["admission"] == "admit" else "BLOCKED"
    if "UNAVAILABLE" in statuses:
        return "UNAVAILABLE"
    return "FAIL"


def validate_receipt(receipt: dict, pins: dict, allow_synthetic: bool = False) -> tuple[str, list[str]]:
    problems: list[str] = []
    if receipt.get("schema") != RECEIPT_SCHEMA:
        return "INVALID", ["schema is not " + RECEIPT_SCHEMA]
    if receipt.get("synthetic") and not allow_synthetic:
        return "INVALID", ["synthetic receipts cannot be accepted"]
    try:
        pin = pin_for(pins, receipt.get("row"))
    except SystemExit as error:
        return "INVALID", [str(error)]
    for field in ("package", "version", "tarball_sha256"):
        if receipt.get(field) != pin[field]:
            problems.append(f"{field} does not match the pin")
    if receipt.get("os") not in OSES:
        problems.append("os is not one of " + ",".join(OSES))
    steps = receipt.get("steps")
    if not isinstance(steps, list) or not steps:
        return "INVALID", problems + ["no steps recorded"]
    seen: dict[str, dict] = {}
    for step in steps:
        name = step.get("id")
        if name not in STEP_LEVEL:
            problems.append(f"unknown step {name}")
            continue
        if name in seen:
            problems.append(f"duplicate step {name}")
            continue
        seen[name] = step
        if step.get("status") not in STATUSES:
            problems.append(f"{name}: bad status")
        if step.get("level") != STEP_LEVEL[name]:
            problems.append(f"{name}: level must be {STEP_LEVEL[name]}")
        if step.get("status") == "PASS":
            if not step.get("evidence"):
                problems.append(f"{name}: PASS without evidence")
            if step.get("count", 0) < 1:
                problems.append(f"{name}: PASS with zero observed events")
        if step.get("status") == "UNAVAILABLE" and step.get("unavailable") not in UNAVAILABLE_KINDS:
            problems.append(f"{name}: UNAVAILABLE must name platform, provider, desktop or artifact")
    for name in required_steps(pin):
        if name not in seen:
            problems.append(f"required step missing: {name}")
    if pin["admission"] != "admit" and seen.get("admission.refused", {}).get("status") == "PASS":
        message = (seen["admission.refused"].get("evidence") or {}).get("message", "")
        if refusal_needle(pin) not in message:
            problems.append("admission.refused: refusal reason does not match the pinned basis")
    engine = receipt.get("engine") or {}
    if not engine.get("sha256") or not engine.get("version"):
        problems.append("engine identity is incomplete")
    if problems:
        return "INVALID", problems
    verdict = verdict_of(pin, steps)
    if verdict in ("PASS", "BLOCKED") and engine.get("kind") != "installed":
        problems.append("an accepted row needs the installed Engine artifact, not a source build")
    if verdict in ("PASS", "BLOCKED") and "(dev)" in str(engine.get("version")):
        problems.append("an accepted row cannot come from a dev build")
    if receipt.get("verdict") != verdict:
        problems.append(f"claimed verdict {receipt.get('verdict')} differs from computed {verdict}")
    if problems:
        return "INVALID", problems
    return verdict, []


def step(name: str, status: str, detail: str, evidence=None, count: int = 0, unavailable: str | None = None) -> dict:
    record = {"id": name, "level": STEP_LEVEL[name], "status": status, "detail": detail, "count": count,
              "evidence": evidence or {}, "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    if unavailable:
        record["unavailable"] = unavailable
    return record


class Engine:
    def __init__(self, binary: Path, home: Path, workspace: Path, flags: list[str], token: str):
        self.binary, self.home, self.workspace, self.flags, self.token = binary, home, workspace, flags, token
        self.port = 0
        self.proc: subprocess.Popen | None = None
        self.log = home.parent / "engine.log"

    def env(self) -> dict:
        env = dict(os.environ)
        env.update({"CODEWHALE_HOME": str(self.home), "HOME": str(self.home), "USERPROFILE": str(self.home),
                    "CODEWHALE_RUNTIME_TOKEN": self.token})
        return env

    def start(self) -> None:
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            self.port = probe.getsockname()[1]
        out = self.log.open("ab")
        command = [str(self.binary), "app-server", "--http", "--port", str(self.port)]
        for flag in self.flags:
            command += ["--enable", flag]
        self.proc = subprocess.Popen(command, cwd=self.workspace, env=self.env(), stdout=out, stderr=subprocess.STDOUT)
        deadline = time.time() + 90
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"Engine exited with {self.proc.returncode} before listening")
            if self.request("GET", "/v1/apps/plugins")[0] == 200:
                return
            time.sleep(0.5)
        raise RuntimeError("Engine did not answer /v1/apps/plugins within 90 s")

    def stop(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(20)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()

    def request(self, method: str, path: str, body: dict | None = None) -> tuple[int, dict]:
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", data=data, method=method)
        req.add_header("Authorization", "Bearer " + self.token)
        if data is not None:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=60) as response:
                raw = response.read()
                return response.status, (json.loads(raw) if raw else {})
        except urllib.error.HTTPError as error:
            raw = error.read()
            try:
                return error.code, json.loads(raw)
            except ValueError:
                return error.code, {"raw": raw.decode("utf-8", "replace")[:500]}
        except OSError:
            return 0, {}


def extract_tarball(tarball: Path, target: Path) -> Path:
    root = target.resolve()
    with tarfile.open(tarball) as archive:
        members = archive.getmembers()
        for member in members:
            destination = (root / member.name).resolve()
            if not (destination == root or root in destination.parents) or member.issym() or member.islnk():
                raise ValueError(f"unsafe archive member {member.name}")
        archive.extractall(root, members=members)
    candidates = sorted(path.parent for path in root.glob("*/package.json"))
    if len(candidates) != 1:
        raise ValueError(f"expected one package directory, found {len(candidates)}")
    return candidates[0]


def view(plugin: dict) -> dict:
    keys = ("state", "enabled", "trust_status", "active", "compatibility", "content_hash", "capability_hash")
    return {key: plugin.get(key) for key in keys}


def find_tarball(pin: dict, tarball_dir: Path) -> Path:
    path = tarball_dir / pin["tarball"]
    if not path.is_file():
        raise FileNotFoundError(f"pinned tarball not found: {path}")
    return path


def run_row(args, pins: dict) -> dict:
    pin = pin_for(pins, args.row)
    binary = Path(args.engine).expanduser().resolve()
    steps: list[dict] = []
    receipt = {"schema": RECEIPT_SCHEMA, "row": pin["row"], "package": pin["package"], "version": pin["version"],
               "tarball_sha256": pin["tarball_sha256"], "os": os_name(), "arch": platform.machine().lower(),
               "flags": list(args.enable), "steps": steps,
               "provider_calls_allowed": bool(args.allow_provider)}
    version = subprocess.run([str(binary), "--version"], capture_output=True, text=True, timeout=60).stdout.strip()
    receipt["engine"] = {"path": str(binary), "sha256": sha256_file(binary), "version": version, "kind": args.engine_kind}
    steps.append(step("engine.identity", "PASS", version, {"sha256": receipt["engine"]["sha256"], "kind": args.engine_kind}, 1))
    steps.append(step("platform.identity", "PASS" if os_name() in OSES else "UNAVAILABLE", os_name(),
                      {"os": os_name(), "arch": receipt["arch"]}, 1, None if os_name() in OSES else "platform"))
    failed: list[str] = []

    def record(item: dict) -> bool:
        steps.append(item)
        if item["status"] == "FAIL":
            failed.append(item["id"])
        return item["status"] == "PASS"

    work = Path(tempfile.mkdtemp(prefix="dsh-acceptance-", dir=args.work_dir))
    engine = None
    try:
        try:
            tarball = find_tarball(pin, Path(args.tarballs))
            actual = sha256_file(tarball)
            ok = actual == pin["tarball_sha256"]
            record(step("pin.tarball_hash", "PASS" if ok else "FAIL", f"sha256 {actual}", {"sha256": actual, "pinned": pin["tarball_sha256"]}, 1))
            if not ok:
                raise ValueError("tarball hash differs from the pin")
            package = extract_tarball(tarball, work / "pkg")
            files = sum(1 for path in package.rglob("*") if path.is_file())
            record(step("package.extract", "PASS" if files else "FAIL", f"{files} files", {"package_dir": package.name, "files": files}, files))
        except Exception as error:
            if "pin.tarball_hash" not in [s["id"] for s in steps]:
                record(step("pin.tarball_hash", "FAIL", str(error)))
            if "package.extract" not in [s["id"] for s in steps]:
                record(step("package.extract", "SKIPPED", "no verified package"))
            raise
        home, workspace = work / "home", work / "workspace"
        home.mkdir()
        workspace.mkdir()
        engine = Engine(binary, home, workspace, list(args.enable), "dsh-acc-" + hashlib.sha256(os.urandom(16)).hexdigest())
        subprocess.run([str(binary), "config", "set", "telemetry", "false"], env=engine.env(), capture_output=True, timeout=60)
        engine.start()
        code, preview = engine.request("POST", "/v1/apps/plugins/import/dsh/preview", {"path": str(package)})
        if pin["admission"] != "admit":
            message = str(preview.get("error", {}).get("message", ""))
            refused = code == 400 and refusal_needle(pin) in message
            record(step("admission.refused", "PASS" if refused else "FAIL", f"HTTP {code}",
                        {"http": code, "message": message[:600], "pinned_verdict": pin["admission"], "pinned_refusal": refusal_needle(pin)}, 1))
            return receipt
        ok = code == 200 and preview.get("content_hash")
        record(step("admission.preview", "PASS" if ok else "FAIL", f"HTTP {code}",
                    {"http": code, "review_hash": preview.get("content_hash"), "error": preview.get("error", {}).get("message", "")[:300],
                     "outcomes": preview.get("conversion", {}).get("outcomes", [])}, 1 if ok else 0))
        if not ok:
            raise RuntimeError("Engine did not admit the pinned package")
        name = preview["conversion"]["plugin_name"]
        code, installed = engine.request("POST", "/v1/apps/plugins/install",
                                         {"source": preview["install_source"], "expected_content_hash": preview["content_hash"]})
        plugin = installed.get("plugin", {})
        ok = code == 201 and plugin.get("state") == "disabled" and plugin.get("trust_status") == "not-reviewed"
        record(step("lifecycle.install", "PASS" if ok else "FAIL", f"HTTP {code}, installed disabled and untrusted: {ok}",
                    {"http": code, "plugin": view(plugin)}, 1 if ok else 0))
        if not ok:
            raise RuntimeError("install failed")
        events = trust_and_enable(engine, name)
        record(events[0])
        record(events[1])
        if failed:
            raise RuntimeError("enable failed")
        provider_unavailable = "provider calls are off; run with --allow-provider and a configured provider"
        for journey in ("journey.core", "journey.controls"):
            if args.allow_provider:
                record(step(journey, "UNAVAILABLE", "no per-row scripted journey exists yet for this row", {}, 0, "artifact"))
            else:
                record(step(journey, "UNAVAILABLE", provider_unavailable, {}, 0, "provider"))
        engine.stop()
        engine.start()
        code, after = engine.request("GET", f"/v1/apps/plugins/{name}")
        ok = code == 200 and after.get("enabled") is True and after.get("trust_status") == "trusted" and after.get("content_hash") == plugin.get("content_hash")
        record(step("lifecycle.restart_retained", "PASS" if ok else "FAIL", f"HTTP {code} after Engine restart",
                    {"http": code, "plugin": view(after), "installed": view(plugin)}, 1 if ok else 0))
        if pin["declares_client"]:
            record(step("client.seats_rendered", "UNAVAILABLE", "the desktop slot-observation contract is not available to this driver; screenshots are not accepted",
                        {"declared_slot_keys": pin["declared_dsh_slot_keys"]}, 0, "desktop"))
        code, _ = engine.request("POST", f"/v1/apps/plugins/{name}/disable")
        code2, revoked = engine.request("POST", f"/v1/apps/plugins/{name}/revoke")
        _, state = engine.request("GET", f"/v1/apps/plugins/{name}")
        ok = code == 200 and code2 == 200 and state.get("active") is False and state.get("trust_status") != "trusted"
        record(step("lifecycle.revoke", "PASS" if ok else "FAIL", f"disable HTTP {code}, revoke HTTP {code2}",
                    {"disable": code, "revoke": code2, "plugin": view(state)}, 1 if ok else 0))
        code, _ = engine.request("DELETE", f"/v1/apps/plugins/{name}")
        code2, _ = engine.request("GET", f"/v1/apps/plugins/{name}")
        record(step("lifecycle.uninstall", "PASS" if code == 200 and code2 == 404 else "FAIL", f"delete HTTP {code}, then GET HTTP {code2}",
                    {"delete": code, "get_after": code2}, 1 if code == 200 and code2 == 404 else 0))
        engine.stop()
        hits, scanned = scan_for_secret(engine.token, [engine.log, home])
        record(step("security.no_key_leak", "PASS" if scanned and not hits else "FAIL", f"scanned {scanned} files, {len(hits)} hits",
                    {"files_scanned": scanned, "hits": hits[:5], "sentinel": "runtime bearer token"}, scanned))
    except Exception as error:
        if "lifecycle.install" not in [s["id"] for s in steps] and pin["admission"] == "admit" and "admission.preview" in [s["id"] for s in steps]:
            steps.append(step("lifecycle.install", "FAIL", f"driver stopped: {error}"))
        receipt["driver_error"] = str(error)
    finally:
        if engine:
            engine.stop()
        have = {s["id"] for s in steps}
        for name_ in required_steps(pin):
            if name_ not in have:
                steps.append(step(name_, "SKIPPED", "an earlier step failed or the driver stopped"))
        shutil.rmtree(work, ignore_errors=True)
    return receipt


def trust_and_enable(engine: Engine, name: str) -> list[dict]:
    base = f"/v1/apps/plugins/{name}"
    trusted = None
    rounds = []
    for _ in range(3):
        code, detail = engine.request("GET", base)
        token = (detail.get("review") or {}).get("token")
        if code != 200 or not token:
            return [step("lifecycle.trust", "FAIL", f"no review token, HTTP {code}", {"http": code}),
                    step("lifecycle.enable", "SKIPPED", "trust did not complete")]
        code, answer = engine.request("POST", base + "/trust", {"token": token})
        rounds.append({"trust_http": code})
        if code != 200:
            return [step("lifecycle.trust", "FAIL", f"trust HTTP {code}", {"http": code, "answer": answer}),
                    step("lifecycle.enable", "SKIPPED", "trust did not complete")]
        trusted = step("lifecycle.trust", "PASS", "review token accepted", {"rounds": rounds}, len(rounds))
        code, answer = engine.request("POST", base + "/enable")
        rounds[-1]["enable_http"] = code
        if code == 409 and "capability review" in json.dumps(answer):
            continue
        _, state = engine.request("GET", base)
        ok = code == 200 and state.get("active") is True
        trusted["evidence"]["rounds"] = rounds
        return [trusted, step("lifecycle.enable", "PASS" if ok else "FAIL", f"enable HTTP {code}, active={state.get('active')}",
                              {"http": code, "plugin": view(state), "answer": answer if code != 200 else {}}, 1 if ok else 0)]
    return [trusted or step("lifecycle.trust", "FAIL", "review kept changing"),
            step("lifecycle.enable", "FAIL", "capability review did not settle after three rounds", {"rounds": rounds})]


def scan_for_secret(secret: str, roots: list[Path]) -> tuple[list[str], int]:
    needle = secret.encode()
    hits, scanned = [], 0
    for root in roots:
        paths = [root] if root.is_file() else [p for p in root.rglob("*") if p.is_file()]
        for path in paths:
            try:
                if path.stat().st_size > 64 * 1024 * 1024:
                    continue
                scanned += 1
                if needle in path.read_bytes():
                    hits.append(str(path))
            except OSError:
                continue
    return hits, scanned


def synthetic_receipt(pins: dict, row: int, with_pass_everywhere: bool = True) -> dict:
    pin = pin_for(pins, row)
    steps = []
    for name in required_steps(pin):
        evidence = {"synthetic": True}
        if name == "admission.refused":
            evidence["message"] = "DSH import refused: " + refusal_needle(pin)
        steps.append(step(name, "PASS", "synthetic", evidence, 1))
    return {"schema": RECEIPT_SCHEMA, "synthetic": True, "row": pin["row"], "package": pin["package"], "version": pin["version"],
            "tarball_sha256": pin["tarball_sha256"], "os": "macos", "arch": "arm64",
            "engine": {"path": "/x", "sha256": "0" * 64, "version": "synthetic", "kind": "installed"},
            "steps": steps, "verdict": "BLOCKED" if pin["admission"] != "admit" else "PASS"}


def selftest(pins: dict) -> int:
    results = []

    def check(label: str, expected: str, receipt: dict, fix_claim: bool = True, synthetic_ok: bool = True) -> None:
        if fix_claim and receipt.get("steps"):
            pin = pin_for(pins, receipt["row"])
            ids = {s["id"] for s in receipt["steps"]}
            if all(n in ids for n in required_steps(pin)):
                receipt["verdict"] = verdict_of(pin, receipt["steps"])
        verdict, problems = validate_receipt(receipt, pins, allow_synthetic=synthetic_ok)
        results.append((label, expected, verdict, problems[:2], verdict == expected))

    base = synthetic_receipt(pins, 1)
    check("all required steps pass", "PASS", copy.deepcopy(base))
    missing = copy.deepcopy(base)
    missing["steps"] = [s for s in missing["steps"] if s["id"] != "lifecycle.revoke"]
    check("one required event missing (lifecycle.revoke)", "INVALID", missing)
    empty = copy.deepcopy(base)
    empty["steps"][4]["count"] = 0
    check("PASS with zero observed events (empty selector)", "INVALID", empty)
    bare = copy.deepcopy(base)
    bare["steps"][4]["evidence"] = {}
    check("PASS without evidence", "INVALID", bare)
    provider = copy.deepcopy(base)
    for item in provider["steps"]:
        if item["id"] == "journey.core":
            item.update(status="UNAVAILABLE", unavailable="provider", count=0, evidence={})
    check("provider unavailable is not PASS", "UNAVAILABLE", provider)
    lying = copy.deepcopy(provider)
    lying["verdict"] = "PASS"
    check("claimed PASS over an unavailable step", "INVALID", lying, fix_claim=False)
    failed = copy.deepcopy(base)
    failed["steps"][5].update(status="FAIL")
    check("a failed step is FAIL", "FAIL", failed)
    unnamed = copy.deepcopy(provider)
    for item in unnamed["steps"]:
        item.pop("unavailable", None)
    check("UNAVAILABLE must name what is missing", "INVALID", unnamed)
    wrong = copy.deepcopy(base)
    wrong["tarball_sha256"] = "f" * 64
    check("tarball hash differs from the pin", "INVALID", wrong)
    source = copy.deepcopy(base)
    source["engine"]["kind"] = "source-build"
    check("source build cannot be accepted as installed", "INVALID", source)
    check("synthetic receipt refused in real validation", "INVALID", copy.deepcopy(base), synthetic_ok=False)
    check("refused row passes only as BLOCKED", "BLOCKED", synthetic_receipt(pins, 14))
    reason = synthetic_receipt(pins, 14)
    for item in reason["steps"]:
        if item["id"] == "admission.refused":
            item["evidence"]["message"] = "DSH import refused: hidden file `.gitignore` is not allowed"
    check("refusal for the wrong reason is not BLOCKED", "INVALID", reason)
    dev = copy.deepcopy(base)
    dev["engine"]["version"] = "codewhale 0.10.3 (dev)"
    check("dev build cannot be accepted", "INVALID", dev)
    client = synthetic_receipt(pins, 2)
    client["steps"] = [s for s in client["steps"] if s["id"] != "client.seats_rendered"]
    check("declared client seats cannot be skipped", "INVALID", client)
    width = max(len(r[0]) for r in results)
    bad = 0
    for label, expected, got, problems, ok in results:
        bad += not ok
        print(f"{'ok  ' if ok else 'FAIL'} {label.ljust(width)}  expected={expected} got={got} {problems if got == 'INVALID' else ''}")
    print(f"selftest: {len(results) - bad} passed, {bad} failed, {len(results)} total")
    return 1 if bad else 0


def cmd_validate(args, pins: dict) -> int:
    receipt = json.loads(Path(args.receipt).read_text())
    verdict, problems = validate_receipt(receipt, pins)
    print(json.dumps({"verdict": verdict, "problems": problems, "row": receipt.get("row"), "os": receipt.get("os")}, indent=1))
    return 0 if verdict in ("PASS", "BLOCKED") else 1


def cmd_matrix(args, pins: dict) -> int:
    table: dict = {}
    invalid = []
    for path in sorted(Path(args.receipts).glob("**/*.json")):
        try:
            receipt = json.loads(path.read_text())
        except ValueError:
            continue
        if not isinstance(receipt, dict) or receipt.get("schema") != RECEIPT_SCHEMA:
            continue
        verdict, problems = validate_receipt(receipt, pins)
        if verdict == "INVALID":
            invalid.append({"receipt": str(path), "problems": problems})
            continue
        slot = table.setdefault(receipt["row"], {})
        previous = slot.get(receipt["os"])
        if previous is None or receipt.get("steps", [{}])[0].get("at", "") >= previous["at"]:
            slot[receipt["os"]] = {"verdict": verdict, "receipt": str(path), "at": receipt["steps"][0].get("at", ""),
                                   "engine": receipt["engine"]["sha256"]}
    rows = []
    for pin in pins["rows"]:
        per_os = {}
        for name in OSES:
            entry = table.get(pin["row"], {}).get(name)
            per_os[name] = entry["verdict"] if entry else "UNAVAILABLE(no receipt)"
        values = set(per_os.values())
        overall = "FAIL" if "FAIL" in values else "PASS" if values == {"PASS"} else "BLOCKED" if values == {"BLOCKED"} else "UNAVAILABLE"
        rows.append({"row": pin["row"], "package": pin["package"], "verdict": overall, "os": per_os})
    summary = {}
    for item in rows:
        summary[item["verdict"]] = summary.get(item["verdict"], 0) + 1
    out = {"schema": MATRIX_SCHEMA, "rows": rows, "summary": summary, "invalid_receipts": invalid}
    print(json.dumps(out, indent=1))
    return 0 if summary.get("PASS", 0) + summary.get("BLOCKED", 0) == len(rows) else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run", help="drive one pinned row against an installed Engine")
    run.add_argument("--row", type=int, required=True)
    run.add_argument("--engine", required=True, help="path to the installed codewhale binary")
    run.add_argument("--engine-kind", choices=["installed", "source-build"], required=True)
    run.add_argument("--tarballs", required=True, help="directory holding the pinned tarballs")
    run.add_argument("--enable", action="append", default=[], help="Engine feature to enable (repeatable), e.g. extension_host")
    run.add_argument("--allow-provider", action="store_true", help="permit model-provider calls (spend); off by default")
    run.add_argument("--work-dir", default=None, help="scratch parent directory")
    run.add_argument("--out", required=True, help="receipt path")
    val = sub.add_parser("validate", help="validate one receipt")
    val.add_argument("receipt")
    mat = sub.add_parser("matrix", help="combine receipts into the row x OS table")
    mat.add_argument("receipts")
    sub.add_parser("selftest", help="negative controls for the validator")
    sub.add_parser("pins", help="verify tarball hashes in a directory")
    pins_cmd = sub.choices["pins"]
    pins_cmd.add_argument("--tarballs", required=True)
    args = parser.parse_args()
    pins = load_pins()
    if args.command == "selftest":
        return selftest(pins)
    if args.command == "validate":
        return cmd_validate(args, pins)
    if args.command == "matrix":
        return cmd_matrix(args, pins)
    if args.command == "pins":
        bad = 0
        for pin in pins["rows"]:
            path = Path(args.tarballs) / pin["tarball"]
            ok = path.is_file() and sha256_file(path) == pin["tarball_sha256"]
            bad += not ok
            print(f"row {pin['row']:>2} {'ok  ' if ok else 'FAIL'} {pin['package']}@{pin['version']}")
        print(f"pins: {len(pins['rows']) - bad} ok, {bad} failed")
        return 1 if bad else 0
    receipt = run_row(args, pins)
    receipt["verdict"] = verdict_of(pin_for(pins, args.row), receipt["steps"])
    Path(args.out).write_text(json.dumps(receipt, indent=1))
    verdict, problems = validate_receipt(receipt, pins)
    print(f"row {args.row} {receipt['package']}@{receipt['version']} on {receipt['os']}: driver verdict {receipt['verdict']}, validator {verdict} {problems}")
    for item in receipt["steps"]:
        print(f"  {item['status']:<11} {item['id']:<28} {item['detail'][:110]}")
    return 0 if verdict in ("PASS", "BLOCKED") else 1


if __name__ == "__main__":
    sys.exit(main())
