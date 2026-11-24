#!/usr/bin/env python3
"""Validate the maintained crates.io publication order against Cargo metadata.

On success, emit the workspace inventory consumed by publish-crates.sh.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


INDEPENDENT_VERSION_CRATES = {"codewhale-ratatui"}


class ValidationError(Exception):
    """A release-order or Cargo metadata contract violation."""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--metadata-file",
        type=Path,
        help="Read Cargo metadata from this file instead of invoking cargo (tests only).",
    )
    parser.add_argument("crates", nargs="+", help="Maintained publication order")
    return parser.parse_args()


def load_metadata(metadata_file: Path | None) -> dict[str, Any]:
    if metadata_file is not None:
        try:
            raw = metadata_file.read_text(encoding="utf-8")
        except OSError as error:
            raise ValidationError(f"could not read Cargo metadata: {error}") from error
    else:
        process = subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"],
            check=False,
            capture_output=True,
            text=True,
        )
        if process.returncode != 0:
            detail = process.stderr.strip()
            suffix = f": {detail}" if detail else ""
            raise ValidationError(
                f"cargo metadata failed with exit code {process.returncode}{suffix}"
            )
        raw = process.stdout

    try:
        metadata = json.loads(raw)
    except json.JSONDecodeError as error:
        raise ValidationError(f"Cargo metadata is not valid JSON: {error}") from error
    if not isinstance(metadata, dict):
        raise ValidationError("Cargo metadata root must be an object")
    return metadata


def workspace_packages(metadata: dict[str, Any]) -> list[dict[str, Any]]:
    members = metadata.get("workspace_members")
    packages = metadata.get("packages")
    if not isinstance(members, list) or not isinstance(packages, list):
        raise ValidationError("Cargo metadata must contain workspace_members and packages lists")

    packages_by_id = {
        package.get("id"): package
        for package in packages
        if isinstance(package, dict) and isinstance(package.get("id"), str)
    }
    missing_ids = [member for member in members if member not in packages_by_id]
    if missing_ids:
        raise ValidationError(
            "Cargo metadata omits workspace package ids: " + ", ".join(missing_ids)
        )
    return [packages_by_id[member] for member in members]


def validate_order(
    packages: list[dict[str, Any]], ordered_crates: list[str]
) -> tuple[str, dict[str, bool], dict[str, str]]:
    duplicate_crates = sorted(
        {name for name in ordered_crates if ordered_crates.count(name) > 1}
    )
    if duplicate_crates:
        raise ValidationError(
            "publish package list contains duplicates: " + ", ".join(duplicate_crates)
        )

    names = [package.get("name") for package in packages]
    if any(not isinstance(name, str) or not name for name in names):
        raise ValidationError("workspace package is missing a name")
    if len(set(names)) != len(names):
        raise ValidationError("Cargo metadata contains duplicate workspace package names")

    independent_versions = {
        package["name"]: package.get("version")
        for package in packages
        if package["name"] in INDEPENDENT_VERSION_CRATES
    }
    for name, own_version in independent_versions.items():
        if not isinstance(own_version, str) or not own_version:
            raise ValidationError(f"{name} has no version")
    versions = sorted(
        {
            package.get("version")
            for package in packages
            if package["name"] not in INDEPENDENT_VERSION_CRATES
        }
    )
    if len(versions) != 1 or not isinstance(versions[0], str) or not versions[0]:
        rendered = ", ".join(str(version) for version in versions)
        raise ValidationError(f"workspace packages have mixed versions: {rendered}")

    workspace_by_name = {package["name"]: package for package in packages}
    # `cargo metadata` renders `publish = false` as an empty registry list.
    # Such a crate cannot be published, so demanding it appear in the publish
    # order is a contradiction: `crates.sh` drives `cargo publish`, which would
    # refuse it. Vendored, workspace-internal crates live here.
    release_names = sorted(
        name
        for name, package in workspace_by_name.items()
        if name.startswith("codewhale-") and package.get("publish") != []
    )
    ordered_set = set(ordered_crates)
    missing = sorted(set(release_names) - ordered_set)
    extra = sorted(ordered_set - set(release_names))
    if missing or extra:
        messages = []
        if missing:
            messages.append("publish package list is missing workspace crates: " + " ".join(missing))
        if extra:
            messages.append(
                "publish package list contains non-workspace crates: " + " ".join(extra)
            )
        raise ValidationError("\n".join(messages))

    positions = {name: index for index, name in enumerate(ordered_crates)}
    has_workspace_dependencies = {name: False for name in release_names}
    publish_edges: set[tuple[str, str, str]] = set()
    for dependent in release_names:
        dependencies = workspace_by_name[dependent].get("dependencies", [])
        if not isinstance(dependencies, list):
            raise ValidationError(f"Cargo metadata dependencies for {dependent} must be a list")
        for dependency in dependencies:
            if not isinstance(dependency, dict) or dependency.get("path") is None:
                continue
            dependency_name = dependency.get("name")
            if dependency_name not in workspace_by_name:
                continue
            has_workspace_dependencies[dependent] = True
            kind = dependency.get("kind") or "normal"
            # Cargo does not compile dev-dependencies while verifying a publish.
            # They may legitimately point back across the publication DAG.
            if kind == "dev":
                continue
            if dependency_name not in positions:
                raise ValidationError(
                    f"{dependent} depends on workspace crate {dependency_name} "
                    f"[{kind}], which is not in the codewhale-* release inventory"
                )
            publish_edges.add((dependency_name, dependent, str(kind)))

    violations = sorted(
        (
            dependency,
            dependent,
            kind,
        )
        for dependency, dependent, kind in publish_edges
        if positions[dependency] >= positions[dependent]
    )
    if violations:
        lines = ["crate publication order is not topological:"]
        for dependency, dependent, kind in violations:
            lines.append(
                f"  {dependent} (position {positions[dependent] + 1}) depends on "
                f"{dependency} (position {positions[dependency] + 1}) [{kind}]"
            )
        lines.append(
            "Move every workspace dependency before its dependent in "
            "scripts/release/crates.sh."
        )
        raise ValidationError("\n".join(lines))

    return versions[0], has_workspace_dependencies, independent_versions


def main() -> int:
    args = parse_args()
    try:
        metadata = load_metadata(args.metadata_file)
        packages = workspace_packages(metadata)
        version, dependency_flags, independent_versions = validate_order(packages, args.crates)
    except ValidationError as error:
        print(error, file=sys.stderr)
        return 1

    print(f"version\t{version}\t")
    for name in sorted(independent_versions):
        print(f"own-version\t{name}\t{independent_versions[name]}")
    build_directory = metadata.get("build_directory") or metadata.get("target_directory")
    if isinstance(build_directory, str) and build_directory:
        print(f"build-dir\t{build_directory}\t")
    for name in sorted(dependency_flags):
        print(f"crate\t{name}\t{1 if dependency_flags[name] else 0}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
