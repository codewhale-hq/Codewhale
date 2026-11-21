import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

scratch = Path(tempfile.mkdtemp(dir=os.environ["RUNNER_TEMP"]))
out = Path(os.environ["RUNNER_TEMP"]) / "proof"
out.mkdir(exist_ok=True)
payload, older, old_archive = map(Path, sys.argv[1:])
pinned = os.environ["BASELINE_ARCHIVE_SHA256"]


def digest(p):
    with p.open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


assert digest(old_archive) == pinned
binary = payload / "codewhale"
native = {"source": os.environ["ENGINE_SHA"], "sha256": digest(binary)}
installer = payload / "install.sh"
home = scratch / "home"
home.mkdir(mode=0o700)
state = home / ".codewhale"
state.mkdir(mode=0o700)
env = {
    "HOME": str(home),
    "CODEWHALE_HOME": str(state),
    "TMPDIR": str(scratch),
    "PATH": "/usr/bin:/bin",
    "LANG": "C.UTF-8",
    "CODEWHALE_NO_UPDATE_CHECK": "1",
    "CODEWHALE_TELEMETRY": "0",
    "CODEWHALE_DISABLE_MODELS_DEV_FETCH": "1",
}


def run(label, *argv):
    result = subprocess.run(
        list(map(str, argv)),
        env=env,
        cwd=home,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    (out / (label + ".log")).write_text(result.stdout)
    assert result.returncode == 0, (label, result.returncode, result.stdout[-2000:])
    return result.stdout


bin = home / ".local/bin"
bin.mkdir(mode=0o700, parents=True)
(bin / "unrelated").write_text("keep\n")
(state / "sentinel").write_text("keep\n")
(home / ".zshrc").write_text("keep\n")


def observe(label):
    receipt = bin / ".codewhale-install.sha256"
    hashes = {n: digest(bin / n) for n in ["codewhale", "codew"]}
    claims = (
        {
            entry.split()[1]: entry.split()[0]
            for entry in receipt.read_text().splitlines()
        }
        if receipt.is_file()
        else {}
    )
    return {
        "versions": {
            n: run(label + "-" + n, bin / n, "--version").strip() for n in hashes
        },
        "sha256": hashes,
        "receipt_exists": receipt.exists(),
        "receipt": claims,
        "matches": bool(claims) and claims == hashes,
    }


def manifests():
    return list((state / "recovery").glob("*/manifest.tsv"))


def rows(p):
    return [
        dict(zip(["original", "stored", "sha256", "size"], line.split("\t")))
        for line in p.read_text().splitlines()
    ]


run("install-baseline", "/bin/bash", older / "install.sh", "--no-modify-path")
(bin / "codew").unlink()
old_version = run("old-version", bin / "codewhale", "--version").strip()
before_sha = digest(bin / "codewhale")
assert (
    "(ff466b2ed0bf)" in old_version and not (bin / ".codewhale-install.sha256").exists()
)
run("install-native", "/bin/bash", payload / "install.sh", "--no-modify-path")
new = observe("upgraded")
assert new["matches"] and native["source"][:12] in new["versions"]["codewhale"]
(original,) = manifests()
assert [Path(r["original"]).name for r in rows(original)] == ["codewhale"]
old_rows = rows(original)
later_receipt = (bin / ".codewhale-install.sha256").read_bytes()
run(
    "partial-restore",
    payload / "codewhale",
    "doctor",
    "--restore",
    original.parent.name,
)
partial = observe("partial")
assert partial["versions"] == {
    "codewhale": old_version,
    "codew": new["versions"]["codew"],
}
assert (
    partial["receipt"] == {"codew": new["sha256"]["codew"]}
    and partial["sha256"]["codewhale"] == before_sha
)
(undo,) = [p for p in manifests() if rows(p)]
undo_rows = undo.read_text().splitlines()
assert {Path(x.split("\t")[0]).name for x in undo_rows} == {
    "codewhale",
    ".codewhale-install.sha256",
}
assert (
    next(x for x in undo_rows if Path(x.split("\t")[0]).name == "codewhale").split(
        "\t"
    )[4]
    == before_sha
)
run("undo-partial", payload / "codewhale", "doctor", "--restore", undo.parent.name)
after_undo = observe("undo")
assert (
    after_undo == new
    and (bin / ".codewhale-install.sha256").read_bytes() == later_receipt
)
(reverse,) = [p for p in manifests() if rows(p)]
run("redo-partial", payload / "codewhale", "doctor", "--restore", reverse.parent.name)
assert observe("partial-again") == partial
run("reinstall", "/bin/bash", payload / "install.sh", "--no-modify-path")
reinstalled = observe("reinstall")
assert (
    reinstalled == new
    and (bin / ".codewhale-install.sha256").read_bytes() == later_receipt
)
assert all(
    p.read_text() == "keep\n"
    for p in (bin / "unrelated", state / "sentinel", home / ".zshrc")
)
result = {
    "source": native["source"],
    "candidate_sha256": native["sha256"],
    "baseline_archive_sha256": pinned,
    "installer_sha256": digest(installer),
    "scratch": str(scratch),
    "baseline": {"codewhale": old_version, "codew": None, "receipt": None},
    "upgraded": new,
    "initial_recovery": old_rows,
    "partial": partial,
    "undo_recovery": undo_rows,
    "undo": after_undo,
    "redo_equals_partial": True,
    "reinstall": reinstalled,
    "unrelated_preserved": True,
}
Path("partial-restore.json").write_text(json.dumps(result, indent=2) + "\n")
print(
    json.dumps(
        {
            "partialClaims": list(partial["receipt"]),
            "undo": after_undo["matches"],
            "reinstall": reinstalled["matches"],
        }
    )
)
