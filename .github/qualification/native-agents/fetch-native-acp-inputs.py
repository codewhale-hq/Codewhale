import base64
import hashlib
import io
import json
import shutil
import tarfile
import urllib.request

from official_input import fetch

dest, target, suffix = fetch()
expected_package = {
    "linux-x64": "SUQS023u46+2+wMUej1Cm8VSFpHUd2c6cO4m2r7suRaCgrdIjR1iXr7ebl31xb/gHzaYS65xnj1fxRqHpzhU1A==",
    "windows-x64": "qCh34hNhX4yaNmOfXZXjxbai5Y/HXgTgWKaTk/5TfDEAs+0QWr9OFiebJU/J3tvZnCLktjsEMAXJsoYwKI2mxg==",
}[target]
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
    with (
        tgz.extractfile(member) as source,
        (dest / ("opencode" + suffix)).open("wb") as output,
    ):
        shutil.copyfileobj(source, output)
for name in ("codewhale", "opencode"):
    (dest / (name + suffix)).chmod(0o755)
print("Verified", package, version, flush=True)
