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
package_url = "https://registry.npmjs.org/@tencent-ai/codebuddy-code/-/codebuddy-code-2.185.0.tgz"
expected_integrity = "tBzQ7taNPmOCbKJpf8d4R/I5BjWGJKkzBFEbYxMCkN6dnWf2INSHz7anN9AC9hsoQZWK79++4JpnmhpNTsGQvA=="
with urllib.request.urlopen(package_url, timeout=120) as source:
    package = source.read()
assert base64.b64encode(hashlib.sha512(package).digest()).decode() == expected_integrity
tarball = dest / "codebuddy-code-2.185.0.tgz"
tarball.write_bytes(package)
with tarfile.open(tarball) as tgz:
    manifest = json.load(tgz.extractfile("package/package.json"))
    assert manifest["name"] == "@tencent-ai/codebuddy-code" and manifest["version"] == "2.185.0"
    assert manifest["bin"]["codebuddy"] == "./bin/codebuddy"
subprocess.run([shutil.which("npm"), "install", "--ignore-scripts", "--no-audit", "--no-fund",
                "--omit=optional", "--prefix", str(dest), str(tarball)], check=True)
print("Verified CodeBuddy package 2.185.0", flush=True)
