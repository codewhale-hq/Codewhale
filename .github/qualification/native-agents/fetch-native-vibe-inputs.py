import hashlib
import json
import shutil
import subprocess
import urllib.request

from official_input import fetch

dest, target, suffix = fetch()
(dest / ("codewhale" + suffix)).chmod(0o755)

release = json.load(
    urllib.request.urlopen("https://pypi.org/pypi/mistral-vibe/3.1.4/json")
)
wheel = next(
    f for f in release["urls"] if f["filename"].endswith("manylinux_2_28_x86_64.whl")
)
expected = "e9bac639bbcdb0cdf3b79f26e6751d0c1651958a6401ac463a7e4c678a8a31ef"
assert wheel["digests"]["sha256"] == expected and wheel["url"].startswith(
    "https://files.pythonhosted.org/"
)
wheel_path = dest / wheel["filename"]
with urllib.request.urlopen(wheel["url"]) as response, wheel_path.open("wb") as output:
    shutil.copyfileobj(response, output)
assert hashlib.sha256(wheel_path.read_bytes()).hexdigest() == expected
python = shutil.which("python3.12")
assert python
subprocess.run([python, "-m", "venv", str(dest / "venv")], check=True)
subprocess.run(
    [str(dest / "venv/bin/pip"), "install", "--no-cache-dir", str(wheel_path)],
    check=True,
)
