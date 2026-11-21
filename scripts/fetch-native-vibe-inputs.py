import base64
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import urllib.request
import zipfile

windows = platform.system() == "Windows"
target = "windows-x64" if windows else "linux-x64"
suffix = ".exe" if windows else ""
artifact_id, expected_zip = {
    "linux-x64": ("14696139237", "8974ab62ba58507bbd8d53b2014084a1e5710c1e856d69e71daf65f1db856e19"),
    "windows-x64": ("14698471209", "e462ffea9dd35fbc9b789f9c710e913419b2483cc71398f52bdb1254de88c78b"),
}[target]

dest = Path(os.environ["RUNNER_TEMP"]) / "native-acp-bin"
dest.mkdir()
api = f"repos/codewhale-hq/Codewhale/actions/artifacts/{artifact_id}"
meta = json.loads(subprocess.check_output(["gh", "api", api]))
assert meta["workflow_run"]["id"] == 44876164458 and meta["workflow_run"]["head_sha"] == "555322f9cbbd2c2fa197abe3263dc43f9e6f2532", meta
artifact_name = "codewhale-" + target + suffix
assert meta["name"] == artifact_name and meta["digest"] == "sha256:" + expected_zip
archive = dest / "engine.zip"
with archive.open("wb") as output:
    subprocess.run(["gh", "api", api + "/zip"], stdout=output, check=True)
with archive.open("rb") as source:
    assert hashlib.file_digest(source, "sha256").hexdigest() == expected_zip
with zipfile.ZipFile(archive) as zf:
    print("Verified RC archive", artifact_name, zf.namelist(), flush=True)
    candidates = [info for info in zf.infolist() if Path(info.filename).name in {artifact_name, "codewhale" + suffix}]
    assert len(candidates) == 1, zf.namelist()
    with zf.open(candidates[0]) as source, (dest / ("codewhale" + suffix)).open("wb") as output:
        shutil.copyfileobj(source, output)


(dest / ("codewhale" + suffix)).chmod(0o755)

release = json.load(urllib.request.urlopen("https://pypi.org/pypi/mistral-vibe/3.1.4/json"))
wheel = next(f for f in release["urls"] if f["filename"].endswith("manylinux_2_28_x86_64.whl"))
expected = "e9bac639bbcdb0cdf3b79f26e6751d0c1651958a6401ac463a7e4c678a8a31ef"
assert wheel["digests"]["sha256"] == expected and wheel["url"].startswith("https://files.pythonhosted.org/")
wheel_path = dest / wheel["filename"]
with urllib.request.urlopen(wheel["url"]) as response, wheel_path.open("wb") as output:
    shutil.copyfileobj(response, output)
assert hashlib.sha256(wheel_path.read_bytes()).hexdigest() == expected
python = shutil.which("python3.12")
assert python
subprocess.run([python, "-m", "venv", str(dest / "venv")], check=True)
subprocess.run([str(dest / "venv/bin/pip"), "install", "--no-cache-dir", str(wheel_path)], check=True)
