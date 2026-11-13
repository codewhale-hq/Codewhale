#!/usr/bin/env python3
"""Everyone whose work landed in or is credited by Codewhale, on one wall.

GitHub's contributor count takes only commit authors and co-author trailers
whose email is linked to an account. Harvested pull requests without a trailer,
unlinked commit emails and credited bug reports never reach it, so it shows
about half of the people who built Codewhale.

  refresh  rebuild docs/contributors.json from git history, the CHANGELOG,
           docs/CONTRIBUTORS.md and GitHub (needs `gh`)
  render   draw docs/assets/contributors.png and the "Everyone" list in
           docs/CONTRIBUTORS.md from docs/contributors.json (offline except
           for avatar downloads, which are cached)

Usage: scripts/contributor-wall.py refresh|render
"""
from __future__ import annotations

import concurrent.futures
import io
import json
import re
import subprocess
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPO = "codewhale-hq/Codewhale"
SIBLINGS = ["codewhale-cu-plugin", "codewhale-ratatui", "codewhale-plugin-marketplace", "CodeWhale-VSCode"]
DATA = ROOT / "docs" / "contributors.json"
WALL = ROOT / "docs" / "assets" / "contributors.png"
CACHE = ROOT / "target" / "contributor-avatars"
NOREPLY = re.compile(r"^(?:(\d+)\+)?([A-Za-z0-9-]+)@users\.noreply\.github\.com$", re.I)
NOT_PEOPLE = re.compile(
    r"\[bot\]|^bot$|copilot|dependabot|github-actions|^claude|anthropic|codex|cursor|devin|jules|grok|deepseek-v4|"
    r"^codewhale|openclaw|autoghclaw|nanookclaw|agentmail\.to|happy\.engineering|codalume|g1 agent|^bot_|"
    r"^hmbown$|^hunter b(own)?$|^username$|^handle$|^host$", re.I)


def sh(*args: str) -> str:
    return subprocess.run(args, cwd=ROOT, capture_output=True, text=True, check=True).stdout


def gh_json(*args: str):
    out = subprocess.run(["gh", *args], cwd=ROOT, capture_output=True, text=True, check=True).stdout
    return [json.loads(line) for line in out.splitlines() if line.strip()]


def author_map() -> dict[str, str]:
    out = {}
    for line in (ROOT / ".github" / "AUTHOR_MAP").read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if "=" in line:
            alias, target = (p.strip() for p in line.split("=", 1))
            m = re.search(r"\d+\+([^@<>\s]+)@users\.noreply\.github\.com", target)
            if m:
                out[alias.lower()] = m.group(1)
    return out


def users_exist(logins: set[str]) -> dict[str, dict]:
    found, logins = {}, sorted(logins)
    for i in range(0, len(logins), 80):
        chunk = logins[i:i + 80]
        query = "query{" + " ".join(f"u{j}: user(login: {json.dumps(x)}) {{ login name }}" for j, x in enumerate(chunk)) + "}"
        out = subprocess.run(["gh", "api", "graphql", "-f", f"query={query}"], cwd=ROOT, capture_output=True, text=True).stdout
        data = json.loads(out or "{}").get("data") or {}
        for j, x in enumerate(chunk):
            if data.get(f"u{j}"):
                found[x.lower()] = data[f"u{j}"]
    return found


def refresh() -> None:
    amap = author_map()
    sources: dict[str, set[str]] = {}
    names: dict[str, set[str]] = {}
    ids: dict[str, str] = {}

    def credit(login: str, why: str) -> None:
        if login and not NOT_PEOPLE.search(login):
            sources.setdefault(login, set()).add(why)

    def person(name: str, email: str, why: str) -> None:
        if NOT_PEOPLE.search(name) or NOT_PEOPLE.search(email) or email.lower().endswith(("anthropic.com", "openai.com")):
            return
        handle = amap.get(email.lower()) or amap.get(name.lower())
        m = NOREPLY.match(email)
        if m and m.group(1):
            ids[m.group(2).lower()] = m.group(1)
        if handle or m:
            credit(handle or m.group(2), why)
        else:
            names.setdefault(name.strip(), set()).add(why)

    for line in sh("git", "log", "--format=%an\t%ae").splitlines():
        person(*line.split("\t", 1), "commit")
    for name, email in re.findall(r"(?im)^co-authored-by:\s*(.+?)\s*<([^>]+)>", sh("git", "log", "--format=%b")):
        person(name, email, "co-author")
    for handle in re.findall(r"(?i)harvested from (?:pr )?#?\d+ by @([A-Za-z0-9-]+)", sh("git", "log", "--format=%B")):
        credit(handle, "harvested pull request")
    record = (ROOT / "docs" / "CONTRIBUTORS.md").read_text().split("## Contributors by time", 1)[-1]
    for handle in re.findall(r"\]\(https://github\.com/([A-Za-z0-9-]+)\)", record):
        credit(handle, "contributor record")
    changelog = (ROOT / "CHANGELOG.md").read_text() + (ROOT / "crates" / "tui" / "CHANGELOG.md").read_text()
    for handle in re.findall(r"(?<![A-Za-z0-9._%+/-])@([A-Za-z0-9][A-Za-z0-9-]{0,38})(?![A-Za-z0-9-]*\.[A-Za-z]{2,})(?![A-Za-z0-9-]*/)", changelog):
        if not handle.isdigit():
            credit(handle, "changelog credit")
    for repo in [REPO] + [f"codewhale-hq/{r}" for r in SIBLINGS]:
        for row in gh_json("api", f"repos/{repo}/contributors?per_page=100", "--paginate", "--jq", ".[] | {login, type}"):
            if row["type"] == "User":
                credit(row["login"], "commit")
        for row in gh_json("api", f"repos/{repo}/pulls?state=closed&per_page=100", "--paginate", "--jq",
                           ".[] | select(.merged_at != null) | {login: .user.login, type: .user.type}"):
            if row["type"] == "User":
                credit(row["login"], "merged pull request")
    known = users_exist(set(sources))
    for login in set(sources):
        uid = ids.get(login.lower())
        if login.lower() not in known and uid:
            out = subprocess.run(["gh", "api", f"/user/{uid}"], cwd=ROOT, capture_output=True, text=True).stdout
            user = json.loads(out or "{}")
            if user.get("login"):
                known[login.lower()] = {"login": user["login"], "name": user.get("name")}
    people: dict[str, set[str]] = {}
    for login, why in sources.items():
        user = known.get(login.lower())
        if user:
            people.setdefault(user["login"], set()).update(why)
    display = {re.sub(r"[^a-z0-9]", "", k.lower()): k for k in people}
    display.update({re.sub(r"[^a-z0-9]", "", (u.get("name") or "").lower()): u["login"]
                    for u in known.values() if u.get("name") and u["login"] in people})
    aliases = json.loads(DATA.read_text()).get("unlinked_aliases", {}) if DATA.exists() else {}
    unlinked = set()
    for n in names:
        key = re.sub(r"[^a-z0-9]", "", n.lower())
        if key and key in display:
            continue
        n = aliases.get(n, n)
        if n:
            unlinked.add(n)
    data = {
        "generated_by": "scripts/contributor-wall.py refresh",
        "github_contributor_list": sum(1 for v in people.values() if "commit" in v),
        "people": [{"login": k, "credited_for": sorted(v)} for k, v in sorted(people.items(), key=lambda kv: kv[0].lower())],
        "unlinked_commit_names": sorted(unlinked, key=str.lower),
        "unlinked_aliases": aliases,
    }
    DATA.write_text(json.dumps(data, ensure_ascii=False, indent=1) + "\n")
    print(f"{len(people)} accounts and {len(unlinked)} unlinked commit names -> {DATA.relative_to(ROOT)}")


def avatar(login: str) -> Path:
    CACHE.mkdir(parents=True, exist_ok=True)
    path = CACHE / f"{login.lower()}.png"
    if not path.exists():
        with urllib.request.urlopen(f"https://github.com/{login}.png?size=96", timeout=30) as resp:
            path.write_bytes(resp.read())
    return path


def render() -> None:
    from PIL import Image, ImageDraw, ImageFont

    data = json.loads(DATA.read_text())
    logins = [p["login"] for p in data["people"]]
    extra = [n for n in data["unlinked_commit_names"] if n.lower() not in {x.lower() for x in logins}]
    total = len(logins) + len(extra)
    with concurrent.futures.ThreadPoolExecutor(16) as pool:
        paths = list(pool.map(avatar, logins))
    cell, cols, pad, head = 52, 26, 24, 76
    rows = -(-total // cols)
    img = Image.new("RGB", (pad * 2 + cols * cell, head + pad + rows * cell), (5, 16, 28))
    draw = ImageDraw.Draw(img)
    mask = Image.new("L", (44, 44), 0)
    ImageDraw.Draw(mask).ellipse((0, 0, 43, 43), fill=255)
    def face_font(size: int, cjk: bool = False):
        for path in (["/System/Library/Fonts/Hiragino Sans GB.ttc"] if cjk else []) + ["/System/Library/Fonts/HelveticaNeue.ttc"]:
            try:
                return ImageFont.truetype(path, size)
            except OSError:
                continue
        return ImageFont.load_default()

    font, small, small_cjk = face_font(30), face_font(15), face_font(15, cjk=True)
    draw.text((pad, 22), f"{total} people built Codewhale", font=font, fill=(244, 247, 250))
    for i, path in enumerate(paths):
        r, c = divmod(i, cols)
        face = Image.open(io.BytesIO(path.read_bytes())).convert("RGB").resize((44, 44), Image.LANCZOS)
        img.paste(face, (pad + c * cell + 4, head + r * cell + 4), mask)
    for k, name in enumerate(extra, start=len(paths)):
        r, c = divmod(k, cols)
        x, y = pad + c * cell + 4, head + r * cell + 4
        draw.ellipse((x, y, x + 43, y + 43), fill=(16, 46, 70))
        draw.text((x + 22, y + 22), name[:2], font=small if name.isascii() else small_cjk, fill=(159, 179, 200), anchor="mm")
    WALL.parent.mkdir(parents=True, exist_ok=True)
    img.quantize(256, method=Image.Quantize.MEDIANCUT).save(WALL, optimize=True)

    links = " · ".join(f"[{x}](https://github.com/{x})" for x in logins)
    section = (
        "<!-- contributor-wall:start -->\n## Everyone\n\n"
        f"[![{total} people built Codewhale](assets/contributors.png)](#everyone)\n\n"
        f"{total} people have code, co-authored commits, carried-forward pull requests or credited reports in Codewhale. "
        "GitHub's contributor count includes only commit authors and co-authors whose email is linked to their "
        "account, so it misses many of them. If your commits are missing there, add your commit email to your "
        "GitHub account and they will link.\n\n"
        f"{links}\n\nAlso with us, from commits whose email is not linked to an account: {' · '.join(extra)}.\n"
        "<!-- contributor-wall:end -->\n"
    )
    doc = ROOT / "docs" / "CONTRIBUTORS.md"
    text = doc.read_text()
    if "<!-- contributor-wall:start -->" in text:
        text = re.sub(r"<!-- contributor-wall:start -->.*?<!-- contributor-wall:end -->\n", lambda _: section, text, flags=re.S)
    else:
        text = text.replace("## Organizational thanks", section + "\n## Organizational thanks", 1)
    doc.write_text(text)
    print(f"{total} people -> {WALL.relative_to(ROOT)} and docs/CONTRIBUTORS.md")


if __name__ == "__main__":
    if len(sys.argv) != 2 or sys.argv[1] not in ("refresh", "render"):
        sys.exit(__doc__)
    {"refresh": refresh, "render": render}[sys.argv[1]]()
