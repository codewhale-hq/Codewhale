import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import zipfile

SOURCE = "bc58781ba10c7c6d192f92848b3f6b4cce4fd3a8"
RUN = 44908205009
REPO = "repos/codewhale-hq/Codewhale"


def api(path):
    return json.loads(subprocess.check_output(["gh", "api", REPO + "/" + path]))


def fetch():
    windows = platform.system() == "Windows"
    target = "windows-x64" if windows else "linux-x64"
    suffix = ".exe" if windows else ""
    name = "codewhale-" + target + suffix
    run = api(f"actions/runs/{RUN}")
    assert (
        run["head_sha"] == SOURCE
        and run["path"] == ".github/workflows/release-candidate.yml"
        and run["event"] == "workflow_dispatch"
    )
    jobs = api(f"actions/runs/{RUN}/jobs?per_page=100")["jobs"]
    (producer,) = [j for j in jobs if j["name"] == "artifacts / Build " + target]
    assert producer["conclusion"] == "success", producer["status"]
    candidates = api(f"actions/runs/{RUN}/artifacts?per_page=100")["artifacts"]
    (artifact,) = [a for a in candidates if not a["expired"] and a["name"] == name]
    assert (
        artifact["workflow_run"]["id"] == RUN
        and artifact["workflow_run"]["head_sha"] == SOURCE
    )
    dest = Path(os.environ["RUNNER_TEMP"]) / "native-acp-bin"
    dest.mkdir()
    archive = dest / "engine.zip"
    with archive.open("wb") as f:
        subprocess.run(
            ["gh", "api", REPO + f"/actions/artifacts/{artifact['id']}/zip"],
            stdout=f,
            check=True,
        )
    with archive.open("rb") as f:
        assert (
            "sha256:" + hashlib.file_digest(f, "sha256").hexdigest()
            == artifact["digest"]
        )
    exe = dest / ("codewhale" + suffix)
    with zipfile.ZipFile(archive) as z:
        (member,) = [
            i for i in z.infolist() if Path(i.filename).name in (name, exe.name)
        ]
        with z.open(member) as data, exe.open("wb") as f:
            shutil.copyfileobj(data, f)
    exe.chmod(0o755)
    with exe.open("rb") as f:
        digest = hashlib.file_digest(f, "sha256").hexdigest()
    receipt = {
        "run": RUN,
        "source": SOURCE,
        "target": target,
        "producer": producer["html_url"],
        "artifactId": artifact["id"],
        "actionsZipSHA256": artifact["digest"],
        "binarySHA256": digest,
    }
    (Path(os.environ["RUNNER_TEMP"]) / "official-input.json").write_text(
        json.dumps(receipt, indent=2) + "\n"
    )
    return dest, target, suffix
