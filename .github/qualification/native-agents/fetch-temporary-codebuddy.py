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
package_url = (
    "https://registry.npmjs.org/@tencent-ai/codebuddy-code/-/codebuddy-code-2.185.0.tgz"
)
expected_integrity = "tBzQ7taNPmOCbKJpf8d4R/I5BjWGJKkzBFEbYxMCkN6dnWf2INSHz7anN9AC9hsoQZWK79++4JpnmhpNTsGQvA=="
with urllib.request.urlopen(package_url, timeout=120) as source:
    package = source.read()
assert base64.b64encode(hashlib.sha512(package).digest()).decode() == expected_integrity
tarball = dest / "codebuddy-code-2.185.0.tgz"
tarball.write_bytes(package)
with tarfile.open(tarball) as tgz:
    manifest = json.load(tgz.extractfile("package/package.json"))
    assert (
        manifest["name"] == "@tencent-ai/codebuddy-code"
        and manifest["version"] == "2.185.0"
    )
    assert manifest["bin"]["codebuddy"] == "./bin/codebuddy"
subprocess.run(
    [
        shutil.which("npm"),
        "install",
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
print("Verified CodeBuddy package 2.185.0", flush=True)
