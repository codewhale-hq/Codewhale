#!/usr/bin/env python3
"""Offline tests for scripts/catalog_models_dev.py (#4117)."""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "catalog_models_dev.py"
SEED = ROOT / "crates" / "config" / "assets" / "models_dev.bundled.json"


class CatalogModelsDevScriptTests(unittest.TestCase):
    def test_snapshot_check_validates_offline_seed(self) -> None:
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), "snapshot", "--check", str(SEED)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("ok:", proc.stdout)
        self.assertIn("providers=", proc.stdout)

    def test_scrub_drops_api_key_fields(self) -> None:
        # Import helpers without network.
        sys.path.insert(0, str(ROOT / "scripts"))
        import catalog_models_dev as mod  # type: ignore

        dirty = {
            "models": {},
            "providers": {
                "deepseek": {
                    "api_key": "sk-should-never-persist",
                    "models": {"deepseek-v4-pro": {"id": "deepseek-v4-pro"}},
                }
            },
            "token": "nope",
        }
        clean = mod.strip_sensitive_fields(dirty)
        self.assertNotIn("token", clean)
        self.assertNotIn("api_key", clean["providers"]["deepseek"])
        self.assertIn("models", clean["providers"]["deepseek"])

    def test_ensure_shape_rejects_empty_object(self) -> None:
        sys.path.insert(0, str(ROOT / "scripts"))
        import catalog_models_dev as mod  # type: ignore

        with self.assertRaises(SystemExit):
            mod.ensure_models_dev_shape({}, "test")

    def test_public_document_drops_api_key(self) -> None:
        sys.path.insert(0, str(ROOT / "scripts"))
        import catalog_models_dev as mod  # type: ignore

        dirty = {
            "models": {},
            "providers": {"deepseek": {"api_key": "sk-x", "models": {}}},
            "token": "nope",
        }
        clean = mod.public_models_dev_document(dirty)
        self.assertNotIn("token", clean)
        self.assertNotIn("api_key", clean["providers"]["deepseek"])

    def test_refresh_write_cache_is_rejected_without_writing(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            source = Path(td) / "catalog.json"
            target = Path(td) / "cache.json"
            source.write_text(
                json.dumps({"models": {}, "providers": {}, "api_key": "sk-nope"}),
                encoding="utf-8",
            )
            env = os.environ.copy()
            env["CODEWHALE_MODELS_DEV_PATH"] = str(source)

            proc = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "refresh",
                    "--write-cache",
                    str(target),
                ],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=False,
                env=env,
            )

            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("disk writes are intentionally unsupported", proc.stderr)
            self.assertFalse(target.exists(), "refresh must remain dry-run only")

    def test_public_limit_value_never_echoes_tokens(self) -> None:
        sys.path.insert(0, str(ROOT / "scripts"))
        import catalog_models_dev as mod  # type: ignore

        self.assertEqual(mod.public_limit_value(128000), "128000")
        self.assertEqual(mod.public_limit_value(None), "null")
        self.assertEqual(mod.public_limit_value("sk-this-is-a-token"), "redacted")
        self.assertEqual(mod.public_limit_value({"authorization": "Bearer secret"}), "redacted")
        self.assertEqual(mod.public_limit_value(True), "redacted")

    def test_public_source_label_strips_query_string(self) -> None:
        sys.path.insert(0, str(ROOT / "scripts"))
        import catalog_models_dev as mod  # type: ignore

        self.assertEqual(
            mod.public_source_label("url:https://models.dev/catalog.json?token=sk-leak"),
            "url:https://models.dev/catalog.json",
        )
        self.assertEqual(mod.public_source_label("file:/tmp/catalog.json"), "file:/tmp/catalog.json")

    def test_drift_command_is_gone(self) -> None:
        proc = run_script("drift")
        self.assertNotEqual(proc.returncode, 0)


SPEC_FIXTURE = """
[source]
url = "https://models.dev/catalog.json"

[meta]
role = "NOT a competing source of truth; live Models.dev wins."

[[canonical]]
key = "demo-pro"
upstream = "vendor/demo-pro"

[[providers]]
id = "moonshot"
upstream = "moonshotai"
name = "Moonshot"
env = ["MOONSHOT_API_KEY"]
default = "kimi-k3"
models = [
  "kimi-k3",
  { id = "GLM-5.2", base_model = "demo-pro" },
  { id = "kimi-plan", from = "moonshotai-plan", upstream_id = "kimi-k3-plan" },
  { id = "kimi-old", curated = true },
]

[[curated]]
provider = "moonshot"
id = "kimi-old"
reason = "upstream dropped it; still served"

[curated.row]
name = "Kimi Old"
limit = { context = 1000 }
"""

UPSTREAM_FIXTURE = {
    "models": {
        "vendor/demo-pro": {
            "id": "vendor/demo-pro",
            "name": "Demo Pro",
            "limit": {"context": 1000, "output": 100},
            "benchmarks": [{"name": "x"}],
        }
    },
    "providers": {
        "moonshotai": {
            "id": "moonshotai",
            "api_key": "sk-provider-level-secret",
            "models": {
                "kimi-k3": {
                    "id": "kimi-k3",
                    "limit": {"context": 1048576, "output": 131072},
                    "cost": {"input": 3, "output": 15, "tiers": [{"input": 6}]},
                    "modalities": {"input": ["text", "image"], "output": ["text"]},
                    "description": "public model description",
                    "client_secret": "sk-row-secret",
                },
                "glm-5.2": {"id": "glm-5.2", "reasoning": True},
                "kimi-new": {"id": "kimi-new"},
            },
        },
        "moonshotai-plan": {"id": "moonshotai-plan", "models": {"kimi-k3-plan": {"id": "kimi-k3-plan"}}},
    },
}


def run_script(*args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
        env=env,
    )


class SeedGeneratorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.spec = self.dir / "spec.toml"
        self.lock = self.dir / "lock.json"
        self.out = self.dir / "seed.json"
        self.upstream = self.dir / "upstream.json"
        self.corrections = self.dir / "corrections.json"
        self.descriptors = self.dir / "provider_descriptors.json"
        self.spec.write_text(SPEC_FIXTURE, encoding="utf-8")
        self.descriptors.write_text(
            json.dumps({"providers": [{"id": "moonshot", "catalog_id": "moonshotai", "default_model": "kimi-k3"}]}),
            encoding="utf-8",
        )
        self.upstream.write_text(json.dumps(UPSTREAM_FIXTURE), encoding="utf-8")
        self.corrections.write_text(
            json.dumps(
                {
                    "revision": "t",
                    "reviewed": {"revision": "fixture"},
                    "models": [{"provider": "moonshot", "id": "kimi-k3", "max_output": 131072, "reason": "r"}],
                }
            ),
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def env(self) -> dict[str, str]:
        env = os.environ.copy()
        env["CODEWHALE_MODELS_DEV_PATH"] = str(self.upstream)
        return env

    def lock_cmd(self, *extra: str) -> subprocess.CompletedProcess[str]:
        return run_script(
            "seed", "lock", "--spec", str(self.spec), "--lock", str(self.lock),
            "--corrections", str(self.corrections), "--descriptors", str(self.descriptors), *extra,
            env=self.env(),
        )

    def render_cmd(self, *extra: str) -> subprocess.CompletedProcess[str]:
        return run_script(
            "seed", "render", "--spec", str(self.spec), "--lock", str(self.lock),
            "--out", str(self.out), "--corrections", str(self.corrections),
            "--descriptors", str(self.descriptors), *extra,
        )

    def test_dry_run_writes_nothing(self) -> None:
        proc = self.lock_cmd("--dry-run")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertFalse(self.lock.exists())
        self.assertIn("dry-run", proc.stdout)

    def test_lock_keeps_allowlisted_fields_scrubs_secrets_and_pins_the_document(self) -> None:
        proc = self.lock_cmd()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        text = self.lock.read_text(encoding="utf-8")
        self.assertNotIn("sk-", text)
        lock = json.loads(text)
        expected = hashlib.sha256(self.upstream.read_bytes()).hexdigest()
        self.assertEqual(lock["source"]["sha256"], expected)
        rows = lock["providers"]["moonshotai"]
        self.assertEqual(rows["kimi-k3"]["description"], "public model description")
        self.assertEqual(rows["kimi-k3"]["cost"]["tiers"], [{"input": 6}])
        self.assertEqual(lock["models"]["vendor/demo-pro"]["benchmarks"], [{"name": "x"}])
        # Case-insensitive match keeps upstream's id in the lock.
        self.assertIn("glm-5.2", rows)
        self.assertNotIn("kimi-new", rows, "only referenced rows are pinned")
        self.assertIn("kimi-k3-plan", lock["providers"]["moonshotai-plan"])
        self.assertIn("kimi-new", proc.stdout, "new upstream models are reported")
        # A correction whose value upstream now states is reported as stale.
        self.assertIn("moonshot/kimi-k3: max_output 131072 equals upstream", proc.stdout)

    def test_render_is_deterministic_and_maps_ids(self) -> None:
        self.assertEqual(self.lock_cmd().returncode, 0)
        self.assertEqual(self.render_cmd().returncode, 0)
        first = self.out.read_bytes()
        self.assertEqual(self.render_cmd().returncode, 0)
        self.assertEqual(first, self.out.read_bytes())
        seed = json.loads(first)
        models = seed["providers"]["moonshot"]["models"]
        self.assertEqual(list(models), ["kimi-k3", "GLM-5.2", "kimi-plan", "kimi-old"])
        self.assertEqual(models["GLM-5.2"]["id"], "GLM-5.2", "Codewhale wire id is kept")
        self.assertEqual(models["GLM-5.2"]["base_model"], "demo-pro")
        self.assertTrue(models["kimi-k3"]["default"])
        self.assertNotIn("default", models["GLM-5.2"])
        self.assertEqual(models["kimi-plan"]["id"], "kimi-plan")
        self.assertEqual(models["kimi-old"]["limit"], {"context": 1000})
        self.assertEqual(seed["models"]["demo-pro"]["id"], "demo-pro")
        self.assertIn("1 canonical", seed["_meta"]["coverage"])
        self.assertIn(seed["_meta"]["role"], SPEC_FIXTURE)

    def test_check_fails_with_a_diff_after_a_hand_edit(self) -> None:
        self.assertEqual(self.lock_cmd().returncode, 0)
        self.assertEqual(self.render_cmd().returncode, 0)
        self.assertEqual(self.render_cmd("--check").returncode, 0)
        edited = self.out.read_text(encoding="utf-8").replace("131072", "131073", 1)
        self.out.write_text(edited, encoding="utf-8")
        proc = self.render_cmd("--check")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("131073", proc.stdout)
        self.assertIn("seed render", proc.stderr)

    def test_lock_refuses_missing_rows_and_curated_rows_upstream_now_lists(self) -> None:
        upstream = json.loads(json.dumps(UPSTREAM_FIXTURE))
        del upstream["providers"]["moonshotai"]["models"]["glm-5.2"]
        upstream["providers"]["moonshotai"]["models"]["kimi-old"] = {"id": "kimi-old"}
        self.upstream.write_text(json.dumps(upstream), encoding="utf-8")
        proc = self.lock_cmd()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("does not list GLM-5.2", proc.stderr)
        self.assertIn("kimi-old: curated, but upstream", proc.stderr)
        self.assertFalse(self.lock.exists())

    def test_render_refuses_a_curated_row_present_in_the_lock(self) -> None:
        self.assertEqual(self.lock_cmd().returncode, 0)
        lock = json.loads(self.lock.read_text(encoding="utf-8"))
        lock["providers"]["moonshotai"]["kimi-old"] = {"id": "kimi-old"}
        self.lock.write_text(json.dumps(lock), encoding="utf-8")
        proc = self.render_cmd()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("curated row also present in the lock", proc.stderr)

    def test_spec_refuses_value_overrides_and_bad_defaults(self) -> None:
        for broken, message in [
            (SPEC_FIXTURE.replace('"kimi-k3",', '{ id = "kimi-k3", limit = 5 },', 1), "never restates"),
            (SPEC_FIXTURE.replace('default = "kimi-k3"', 'default = "nope"'), "exactly one default"),
            (SPEC_FIXTURE.replace("curated = true", "base_model = \"x\""), "not marked curated"),
        ]:
            self.spec.write_text(broken, encoding="utf-8")
            proc = self.render_cmd()
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn(message, proc.stderr)


class CommittedSeedTests(unittest.TestCase):
    def test_committed_seed_is_the_rendered_seed(self) -> None:
        proc = run_script("seed", "render", "--check")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    def test_reviewed_source_is_exactly_embedded_without_mutating_lock(self) -> None:
        data = json.loads(SEED.read_text())
        source = json.loads((ROOT / "crates/config/assets/catalog_corrections.json").read_text())
        self.assertEqual(data["_reviewed"], source["reviewed"])
        lock = json.loads((ROOT / "scripts/catalog/models_dev_seed.lock.json").read_text())
        self.assertIn(f"fetched {lock['source']['fetched_at']}", data["_meta"]["upstream"])
        self.assertEqual(len(data["_reviewed"]["public_models"]), 91)

    def test_reviewed_malformed_and_duplicate_public_facts_are_refused(self) -> None:
        sys.path.insert(0, str(ROOT / "scripts"))
        import catalog_models_dev as mod
        data = json.loads((ROOT / "crates/config/assets/catalog_corrections.json").read_text())["reviewed"]
        for mutate in [
            lambda row: row.pop("revision"),
            lambda row: row["intrinsic"]["kimi-k3"].update(context_window=0),
            lambda row: row["intrinsic"]["kimi-k3"].update(max_output=True),
            lambda row: row["public_models"].append(row["public_models"][0]),
            lambda row: row["compatibility_aliases"].update(hostile={"name": "bad\x1bvalue"}),
            lambda row: row["numeric_refs"]["KIMI_K3_CONTEXT_WINDOW_TOKENS"].update(model="missing-private-name"),
        ]:
            fixture = json.loads(json.dumps(data))
            mutate(fixture)
            with self.assertRaises(SystemExit):
                mod.validate_reviewed(fixture)


if __name__ == "__main__":
    unittest.main()
