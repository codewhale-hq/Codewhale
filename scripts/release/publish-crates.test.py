#!/usr/bin/env python3
"""Exercise the upload boundary with real Cargo tarballs and unpublished dependencies."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent


class PublishPreflightTests(unittest.TestCase):
    def setUp(self):
        real_cargo = shutil.which("cargo")
        self.assertIsNotNone(real_cargo, "Cargo 1.90+ must be installed to run release preflight tests")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.scripts = self.root / "scripts/release"
        self.scripts.mkdir(parents=True)
        for name in ("publish-crates.sh", "validate-crate-publish-order.py", "check-crate-sizes.py"):
            shutil.copy2(SCRIPTS / name, self.scripts / name)
        (self.scripts / "crates.sh").write_text(
            "release_crates=(codewhale-preflight-base codewhale-preflight-app)\n"
        )
        # These independently tested guards require a real GitHub release. The
        # fixture tests the subsequent Cargo boundary without contacting GitHub.
        for name in ("require-release-tag-checkout.sh", "verify-release-assets.sh"):
            guard = self.scripts / name
            guard.write_text("#!/bin/sh\nexit 0\n")
            guard.chmod(0o755)
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["base", "app"]\nresolver = "2"\n'
        )
        for document in ("LICENSE", "THIRD_PARTY_NOTICES.md", "docs/THIRD_PARTY_NOTICES.md"):
            source = self.root / document
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text(document)
            for name in ("base", "app"):
                target = self.root / name / "codewhale-notices" / document
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target)
        for name in ("base", "app"):
            crate = self.root / name
            (crate / "src").mkdir(parents=True)
            manifest = (
                f'[package]\nname = "codewhale-preflight-{name}"\n'
                'version = "0.0.0"\nedition = "2021"\nlicense = "MIT"\n'
                'exclude = ["src/payload.txt"]\n'
            )
            if name == "app":
                manifest += (
                    '[dependencies]\ncodewhale-preflight-base = '
                    '{ path = "../base", version = "=0.0.0" }\n'
                )
            (crate / "Cargo.toml").write_text(manifest)
            (crate / "src/lib.rs").write_text('pub const VALUE: &str = "ok";\n')
        (self.root / "app/src/lib.rs").write_text(
            'pub const VALUE: &str = include_str!("payload.txt");\n'
        )
        (self.root / "app/src/payload.txt").write_text("embedded asset\n")
        self.uploads = self.root / "uploads"
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        cargo = bin_dir / "cargo"
        cargo.write_text(
            '#!/usr/bin/env bash\nset -euo pipefail\n'
            'if [[ "${1:-}" == --version && -n "${TEST_CARGO_VERSION:-}" ]]; then\n'
            '  echo "$TEST_CARGO_VERSION"; exit 0\nfi\n'
            # Cargo publish --dry-run still contacts the registry. Keep its real
            # tarball build for the old-script regression check, fully offline.
            'if [[ "${1:-}" == publish ]]; then\n'
            '  shift\n  if [[ " $* " == *" --dry-run "* ]]; then\n'
            '    args=()\n    for arg in "$@"; do\n'
            '      [[ "$arg" == --dry-run ]] || args+=("$arg")\n    done\n'
            '    exec "$TEST_REAL_CARGO" package "${args[@]}"\n  fi\n'
            '  echo attempted >> "$TEST_UPLOADS"\n  exit 98\nfi\n'
            'exec "$TEST_REAL_CARGO" "$@"\n'
        )
        cargo.chmod(0o755)
        curl = bin_dir / "curl"
        curl.write_text("#!/bin/sh\nexit 22\n")
        curl.chmod(0o755)
        self.env = {
            **os.environ,
            "TEST_REAL_CARGO": real_cargo,
            "TEST_UPLOADS": str(self.uploads),
            "PATH": str(bin_dir) + os.pathsep + os.environ["PATH"],
            "CARGO_NET_OFFLINE": "true",
            "CARGO_HOME": str(self.root / "cargo-home"),
            "CARGO_TARGET_DIR": str(self.root / "target"),
        }
        self.run_command(["cargo", "generate-lockfile", "--offline"], success=True)

    def run_command(self, args, *, success):
        result = subprocess.run(
            args, cwd=self.root, env=self.env, capture_output=True, text=True
        )
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode == 0, success, output)
        return output

    def assert_missing_asset_blocks(self, mode):
        # The workspace compiles: only the published tarball loses the asset.
        self.run_command(["cargo", "check", "--locked"], success=True)
        output = self.run_command(
            ["bash", str(self.scripts / "publish-crates.sh"), mode], success=False
        )
        self.assertFalse(self.uploads.exists(), "upload reached before all packages passed")
        self.assertIn("payload.txt", output)

    def test_old_cargo_fails_before_packaging_or_upload(self):
        for version in ("cargo 1.88.0 (fixture)", "cargo 1.89.0 (fixture)", "unknown"):
            with self.subTest(version=version):
                self.env["TEST_CARGO_VERSION"] = version
                output = self.run_command(
                    ["bash", str(self.scripts / "publish-crates.sh"), "publish"], success=False
                )
                self.assertIn("requires Cargo 1.90 or newer", output)
                self.assertFalse(self.uploads.exists())
                self.assertFalse((self.root / "target/package").exists())

    def test_resume_verifies_tarballs_and_skips_existing_versions(self):
        manifest = self.root / "app/Cargo.toml"
        manifest.write_text(manifest.read_text().replace('exclude = ["src/payload.txt"]\n', ""))
        (self.root / "bin/curl").write_text("#!/bin/sh\nexit 0\n")
        output = self.run_command(
            ["bash", str(self.scripts / "publish-crates.sh"), "publish"], success=True
        )
        self.assertIn("Skipping codewhale-preflight-base", output)
        self.assertIn("Skipping codewhale-preflight-app", output)
        self.assertTrue((self.root / "target/package/codewhale-preflight-app-0.0.0.crate").exists())
        self.assertFalse(self.uploads.exists())

    def test_dry_run_builds_dependent_tarball(self):
        self.assert_missing_asset_blocks("dry-run")

    def test_publish_builds_every_tarball_before_first_upload(self):
        self.assert_missing_asset_blocks("publish")

    def test_dry_run_accepts_unpublished_workspace_dependencies(self):
        manifest = self.root / "app/Cargo.toml"
        manifest.write_text(manifest.read_text().replace('exclude = ["src/payload.txt"]\n', ""))
        self.run_command(
            ["bash", str(self.scripts / "publish-crates.sh"), "dry-run"], success=True
        )
        self.assertFalse(self.uploads.exists())

    def test_changed_and_missing_notices_refuse_upload(self):
        manifest = self.root / "app/Cargo.toml"
        manifest.write_text(manifest.read_text().replace('exclude = ["src/payload.txt"]\n', ""))
        (self.root / "app/codewhale-notices/THIRD_PARTY_NOTICES.md").write_text("out of date")
        (self.root / "base/codewhale-notices/docs/THIRD_PARTY_NOTICES.md").unlink()
        output = self.run_command(
            ["bash", str(self.scripts / "publish-crates.sh"), "publish"], success=False
        )
        self.assertFalse(self.uploads.exists())
        self.assertIn("packaged THIRD_PARTY_NOTICES.md differs", output)
        self.assertIn("missing docs/THIRD_PARTY_NOTICES.md", output)

    def test_oversized_tarball_is_refused_before_any_upload(self):
        # crates.io refused codewhale-tui's 11.93 MiB tarball with HTTP 413 in
        # v0.10.1, after 26 crates had already uploaded. An oversized tarball
        # must stop the release before the first upload; incompressible bytes
        # keep this fixture's packed tarball over the real 10 MiB limit.
        manifest = self.root / "app/Cargo.toml"
        manifest.write_text(manifest.read_text().replace('exclude = ["src/payload.txt"]\n', ""))
        (self.root / "app/src/big.bin").write_bytes(os.urandom(11 * 1024 * 1024))
        output = self.run_command(
            ["bash", str(self.scripts / "publish-crates.sh"), "publish"], success=False
        )
        self.assertFalse(self.uploads.exists(), "upload reached before the size gate")
        self.assertIn("codewhale-preflight-app", output)
        self.assertIn("over the crates.io limit", output)
        self.assertIn("10485760", output)


if __name__ == "__main__":
    unittest.main()
