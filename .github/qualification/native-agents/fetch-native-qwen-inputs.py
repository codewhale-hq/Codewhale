import base64
import hashlib
import json
import shutil
import subprocess
import tarfile
import urllib.request

from official_input import fetch

dest, target, suffix = fetch()
(dest / ("codewhale" + suffix)).chmod(0o755)
package_url = "https://registry.npmjs.org/@qwen-code/qwen-code/-/qwen-code-0.21.4.tgz"
expected_integrity = "vZ5jOOVG9Yev0oKxBEscyntgZofHuVqXbBraWFds3bV/Ru1dpQGmwNFJP0fevoR5TLOWNbiOv2DPLz3vhNd0yQ=="
with urllib.request.urlopen(package_url, timeout=120) as source:
    package = source.read()
assert base64.b64encode(hashlib.sha512(package).digest()).decode() == expected_integrity
tarball = dest / "qwen-code-0.21.4.tgz"
tarball.write_bytes(package)
with tarfile.open(tarball) as tgz:
    manifest = json.load(tgz.extractfile("package/package.json"))
    assert (
        manifest["name"] == "@qwen-code/qwen-code" and manifest["version"] == "0.21.4"
    )
    assert not manifest["dependencies"]
subprocess.run(
    [
        shutil.which("npm"),
        "install",
        "--offline",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        "--omit=optional",
        "--prefix",
        str(dest),
        str(tarball),
    ],
    check=True,
)
print("Verified Qwen package 0.21.4", flush=True)
