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
artifact_id, expected_zip, expected_package = {
    "linux-x64": ("14696139237", "8974ab62ba58507bbd8d53b2014084a1e5710c1e856d69e71daf65f1db856e19",
                  "SUQS023u46+2+wMUej1Cm8VSFpHUd2c6cO4m2r7suRaCgrdIjR1iXr7ebl31xb/gHzaYS65xnj1fxRqHpzhU1A=="),
    "windows-x64": ("14698471209", "e462ffea9dd35fbc9b789f9c710e913419b2483cc71398f52bdb1254de88c78b",
                    "qCh34hNhX4yaNmOfXZXjxbai5Y/HXgTgWKaTk/5TfDEAs+0QWr9OFiebJU/J3tvZnCLktjsEMAXJsoYwKI2mxg=="),
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

package = "opencode-" + target
version = "1.19.6"
url = f"https://registry.npmjs.org/{package}/-/{package}-{version}.tgz"
with urllib.request.urlopen(url, timeout=120) as response:
    data = response.read()
assert base64.b64encode(hashlib.sha512(data).digest()).decode() == expected_package
with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tgz:
    meta = json.load(tgz.extractfile("package/package.json"))
    assert meta["name"] == package and meta["version"] == version
    member = tgz.getmember("package/bin/opencode" + suffix)
    assert member.isfile()
    with tgz.extractfile(member) as source, (dest / ("opencode" + suffix)).open("wb") as output:
        shutil.copyfileobj(source, output)
for name in ("codewhale", "opencode"):
    (dest / (name + suffix)).chmod(0o755)
print("Verified", package, version, flush=True)
