#!/usr/bin/env python3
"""Refuse release tarballs that exceed crates.io's 10 MiB upload limit.

crates.io answers an oversized upload with HTTP 413 only when the upload
happens, which can strand a release half-published: v0.10.1 uploaded 26
crates, then codewhale-tui was refused at 11.93 MiB compressed before its
dependent codewhale-cli could follow. The packaging step already builds
every tarball (`cargo publish --dry-run` stages them under
Cargo's build-directory `package/tmp-registry/`, `cargo package` writes
`package/` there); this checks those files so the refusal happens before the
first real upload, at dry-run time as well as publish time.

Usage: check-crate-sizes.py <crate> [<crate> ...]
"""

from __future__ import annotations

import json
import subprocess
import sys
import tarfile
from pathlib import Path

# crates.io's own number, verbatim: "max upload size is: 10485760".
LIMIT_BYTES = 10 * 1024 * 1024
# Notice before the wall: codewhale-tui went from 8.94 MiB (0.10.0) to
# 11.93 MiB (0.10.1, refused) in one cycle. Warn at 90% so a release has a
# cycle to act instead of a failed publish.
WARN_BYTES = LIMIT_BYTES * 90 // 100


def main() -> int:
    crates = sys.argv[1:]
    if not crates:
        print("usage: check-crate-sizes.py <crate> [<crate> ...]", file=sys.stderr)
        return 2

    metadata = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"],
        check=False,
        capture_output=True,
        text=True,
    )
    if metadata.returncode != 0:
        print("::error::cargo metadata failed; cannot locate release tarballs", file=sys.stderr)
        print(metadata.stderr, file=sys.stderr)
        return 1
    parsed = json.loads(metadata.stdout)
    target_directory = Path(parsed.get("build_directory") or parsed["target_directory"])
    versions = {package["name"]: package["version"] for package in parsed["packages"]}

    root = Path(parsed["workspace_root"])
    notices = {name: (root / name).read_bytes() for name in ("LICENSE", "THIRD_PARTY_NOTICES.md", "docs/THIRD_PARTY_NOTICES.md")}
    failures: list[str] = []
    warnings: list[str] = []
    for name in crates:
        version = versions.get(name)
        if version is None:
            failures.append(f"{name}: not a workspace package in cargo metadata")
            continue
        candidates = [
            target_directory / "package" / "tmp-registry" / f"{name}-{version}.crate",
            target_directory / "package" / f"{name}-{version}.crate",
        ]
        tarball = next((candidate for candidate in candidates if candidate.is_file()), None)
        if tarball is None:
            failures.append(
                f"{name}: no tarball at "
                + " or ".join(str(candidate) for candidate in candidates)
                + "; package the release crates first"
            )
            continue
        with tarfile.open(tarball, "r:gz") as archive:
            for document, expected in notices.items():
                member = f"{name}-{version}/codewhale-notices/{document}"
                try:
                    packed = archive.getmember(member)
                except KeyError:
                    failures.append(f"{name}: missing {document} in package")
                    continue
                if not packed.isfile() or archive.extractfile(packed).read() != expected:
                    failures.append(f"{name}: packaged {document} differs from release source")
        size = tarball.stat().st_size
        status = "ok"
        if size > LIMIT_BYTES:
            status = "OVER"
        elif size > WARN_BYTES:
            status = "close"
        print(f"  {status:>5}  {name}-{version}.crate  {size / 1048576:.2f} MiB")
        if status == "OVER":
            failures.append(
                f"{name}: tarball is {size} bytes ({size / 1048576:.2f} MiB), over the "
                f"crates.io limit of {LIMIT_BYTES} bytes (10 MiB). Trim the package "
                "with `exclude` in its Cargo.toml."
            )
        elif status == "close":
            warnings.append(
                f"{name}: tarball is {size} bytes ({size / 1048576:.2f} MiB), "
                f"{size * 100 // LIMIT_BYTES}% of the crates.io cap"
            )

    for warning in warnings:
        print(f"::warning::{warning}", file=sys.stderr)
    for failure in failures:
        print(f"::error::{failure}", file=sys.stderr)
    if failures:
        return 1
    print(
        f"Release tarball sizes OK: all {len(crates)} crates under the "
        f"crates.io {LIMIT_BYTES}-byte cap."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
