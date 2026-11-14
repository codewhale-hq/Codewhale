#!/usr/bin/env python3
"""Replay one sealed CodeWhale candidate without its original worktree or provider."""

import argparse
import hashlib
import json
import os
import resource
import subprocess
import sys
import unicodedata
from pathlib import Path, PurePosixPath


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode()


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"),
                      parse_constant=lambda value: (_ for _ in ()).throw(
                          ValueError(f"non-JSON constant {value}")))


def safe_path(value):
    path = PurePosixPath(value)
    if not value or path.is_absolute() or any(part in ("", ".", "..") for part in value.split("/")) \
            or "\\" in value or str(path) != value:
        raise ValueError(f"unsafe artifact path: {value!r}")
    return value


def verify_paths(paths):
    folded = set()
    for name in paths:
        safe_path(name)
        key = unicodedata.normalize("NFKC", name).casefold()
        if key in folded:
            raise ValueError(f"artifact path collision: {name}")
        folded.add(key)


def materialize(bundle, candidate_id, destination):
    baseline = read_json(bundle / "baseline.json")
    artifact_path = bundle / "artifacts" / f"{candidate_id}.json"
    artifact_bytes = artifact_path.read_bytes()
    artifact = json.loads(artifact_bytes)
    original = read_json(bundle / f"{candidate_id}-evaluation.json")
    if digest(artifact_bytes) != original["artifact_manifest_sha256"]:
        raise ValueError("artifact manifest changed after evaluation")
    if artifact["candidate_id"] != candidate_id or artifact["schema_version"] != 1:
        raise ValueError("artifact identity or schema mismatch")
    if digest(canonical(baseline)) != artifact["baseline_id"]:
        raise ValueError("baseline bundle hash mismatch")
    files = {entry["path"]: entry for entry in baseline}
    verify_paths(files)
    previous = None
    for change in artifact["entries"]:
        name = safe_path(change["path"])
        if previous is not None and name <= previous:
            raise ValueError("artifact changes are not strictly sorted")
        previous = name
        if change["kind"] == "deleted" and change["blob_hash"] is None and change["mode"] is None:
            files.pop(name, None)
        elif change["kind"] == "file":
            files[name] = {field: change[field] for field in
                           ("path", "mode", "size_bytes", "blob_hash")}
        else:
            raise ValueError("unsupported artifact change")
    verify_paths(files)
    tree = [files[name] for name in sorted(files)]
    tree_hash = digest(canonical(tree))
    if tree_hash != artifact["candidate_tree_hash"] or tree_hash != artifact["artifact_id"] \
            or tree_hash != original["artifact_id"]:
        raise ValueError("candidate tree hash mismatch")
    if len(tree) > 4096 or sum(entry["size_bytes"] for entry in tree) > 64 * 1024 * 1024:
        raise ValueError("materialized tree exceeds limits")
    destination.mkdir(parents=True, exist_ok=False)
    for entry in tree:
        if entry["mode"] not in (0o100644, 0o100755) or entry["size_bytes"] > 4 * 1024 * 1024:
            raise ValueError("unsupported file mode or size")
        blob_hash = entry["blob_hash"]
        if len(blob_hash) != 64 or any(char not in "0123456789abcdef" for char in blob_hash):
            raise ValueError("invalid blob hash")
        blob = (bundle / "blobs" / blob_hash).read_bytes()
        if digest(blob) != blob_hash or len(blob) != entry["size_bytes"]:
            raise ValueError(f"blob integrity failure: {entry['path']}")
        target = destination / entry["path"]
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(blob)
        target.chmod(entry["mode"] & 0o777)
    return artifact, original


def same_json(actual, expected):
    if isinstance(actual, bool) or isinstance(expected, bool):
        return type(actual) is type(expected) and actual == expected
    if isinstance(actual, (int, float)) and isinstance(expected, (int, float)):
        if isinstance(actual, float) != isinstance(expected, float):
            integer = actual if isinstance(actual, int) else expected
            return abs(integer) <= 2 ** 53 and actual == expected
        return actual == expected
    if type(actual) is not type(expected):
        return False
    if isinstance(actual, list):
        return len(actual) == len(expected) and all(
            same_json(left, right) for left, right in zip(actual, expected))
    if isinstance(actual, dict):
        return actual.keys() == expected.keys() and all(
            same_json(actual[key], expected[key]) for key in actual)
    return actual == expected


def limits():
    for name, value in ((resource.RLIMIT_AS, 512 * 1024 * 1024),
                        (resource.RLIMIT_FSIZE, 1024 * 1024),
                        (resource.RLIMIT_NOFILE, 64), (resource.RLIMIT_NPROC, 32),
                        (resource.RLIMIT_CPU, 8)):
        resource.setrlimit(name, (value, value))


def case_command(source, scratch, gate):
    argv = ["/usr/bin/bwrap", "--unshare-all", "--die-with-parent",
            "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"]
    for grant in ("/usr", "/lib", "/lib64", "/etc/alternatives"):
        if Path(grant).exists():
            argv += ["--ro-bind", grant, grant]
    argv += ["--ro-bind", str(source), str(source),
             "--bind", str(scratch), str(scratch), "--chdir", str(scratch),
             "--", gate["candidate_argv"][0]]
    argv += [str(source / safe_path(argument)) for argument in gate["candidate_argv"][1:]]
    return argv


def replay(bundle, cases_dir, candidate_id, output, include_holdout):
    manifest = read_json(bundle / "manifest.json")
    source = output / "materialized"
    artifact, original = materialize(bundle, candidate_id, source)
    if manifest["baseline_id"] != artifact["baseline_id"]:
        raise ValueError("frozen manifest and artifact baseline differ")
    if original["status"] != "PASS":
        raise ValueError("only a verified public PASS can be replayed")
    rows = []
    for gate_index, gate in enumerate(manifest["hard_gates"]):
        if gate["holdout"] and not include_holdout:
            continue
        cases_path = cases_dir / f"gate_{gate_index}.cases.json"
        case_bytes = cases_path.read_bytes()
        if digest(case_bytes) != manifest["case_bundle_sha256"][gate_index]:
            raise ValueError(f"trusted case bundle changed: {gate_index}")
        cases = json.loads(case_bytes)
        if not cases or len(cases) < gate["min_tests"]:
            raise ValueError("trusted gate has too few cases")
        passed = 0
        observed = []
        for case_index, case in enumerate(cases):
            scratch = output / f"scratch-g{gate_index}-c{case_index}"
            scratch.mkdir()
            argv = case_command(source, scratch, gate)
            input_bytes = canonical(case["input"])
            stdout_path = output / f"g{gate_index}-c{case_index}.stdout"
            stderr_path = output / f"g{gate_index}-c{case_index}.stderr"
            try:
                with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
                    completed = subprocess.run(
                        argv, input=input_bytes, stdout=stdout, stderr=stderr,
                        cwd=scratch, env={"HOME": str(scratch), "PATH": "/usr/bin:/bin",
                                          "PYTHONDONTWRITEBYTECODE": "1"},
                        preexec_fn=limits, timeout=min(gate["timeout_secs"], 8), check=False)
                try:
                    actual = json.loads(stdout_path.read_bytes(),
                                        parse_constant=lambda value: (_ for _ in ()).throw(
                                            ValueError(f"non-JSON constant {value}")))
                    correct = completed.returncode == 0 and same_json(actual, case["expected"])
                except (UnicodeDecodeError, ValueError):
                    correct = False
                observed.append({"case": case_index + 1, "passed": correct,
                                 "exit_code": completed.returncode})
                passed += int(correct)
            except subprocess.TimeoutExpired:
                observed.append({"case": case_index + 1, "passed": False,
                                 "stop_reason": "timeout"})
        rows.append({"gate": gate_index, "holdout": gate["holdout"],
                     "test_count": len(cases), "passed_count": passed,
                     "status": "PASS" if passed == len(cases) else "FAIL", "cases": observed})
        if passed != len(cases):
            break
    return {"schema_version": 1, "candidate_id": candidate_id,
            "artifact_id": artifact["artifact_id"], "baseline_id": artifact["baseline_id"],
            "source_tree_hash": artifact["candidate_tree_hash"],
            "sandbox_backend": subprocess.check_output(["/usr/bin/bwrap", "--version"], text=True).strip(),
            "status": "PASS" if rows and all(row["status"] == "PASS" for row in rows) else "FAIL",
            "test_count": sum(row["test_count"] for row in rows), "gates": rows}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--evaluator-dir", type=Path, required=True)
    parser.add_argument("--cases-dir", type=Path, required=True)
    parser.add_argument("--candidate-id", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--include-holdout", action="store_true")
    args = parser.parse_args()
    if sys.platform != "linux" or not Path("/usr/bin/bwrap").is_file():
        parser.error("UNSUPPORTED_ISOLATION: Linux bubblewrap is required")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    result = replay(args.evaluator_dir.resolve(), args.cases_dir.resolve(),
                    args.candidate_id, output, args.include_holdout)
    (output / "replay.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"receipt": str(output / "replay.json"), "status": result["status"],
                      "test_count": result["test_count"]}))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
