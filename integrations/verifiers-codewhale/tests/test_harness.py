from __future__ import annotations

import importlib
import json
import sys
import types
import unittest
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace


def install_verifiers_stubs() -> None:
    class HarnessConfig:
        env: dict[str, str] = {}
        disabled_tools: list[str] | None = None

        def __init__(self, **values):
            self.env = dict(values.pop("env", {}))
            self.disabled_tools = values.pop("disabled_tools", None)
            for cls in reversed(type(self).mro()):
                for name in getattr(cls, "__annotations__", {}):
                    if name not in self.__dict__ and hasattr(cls, name):
                        setattr(self, name, getattr(cls, name))
            for name, value in values.items():
                setattr(self, name, value)

        @property
        def resolved_env(self):
            return dict(self.env)

    class Harness:
        @classmethod
        def __class_getitem__(cls, _item):
            return cls

        def __init__(self, config):
            self.config = config

        def resolve_prompt(self, task):
            return task.system_prompt, task.prompt

    @dataclass(frozen=True)
    class ProgramResult:
        exit_code: int
        stdout: str
        stderr: str

    packages = {
        "verifiers": types.ModuleType("verifiers"),
        "verifiers.v1": types.ModuleType("verifiers.v1"),
        "verifiers.v1.clients": types.ModuleType("verifiers.v1.clients"),
        "verifiers.v1.harness": types.ModuleType("verifiers.v1.harness"),
        "verifiers.v1.runtimes": types.ModuleType("verifiers.v1.runtimes"),
        "verifiers.v1.trace": types.ModuleType("verifiers.v1.trace"),
    }
    packages["verifiers.v1.clients"].ModelContext = type("ModelContext", (), {})
    packages["verifiers.v1.harness"].Harness = Harness
    packages["verifiers.v1.harness"].HarnessConfig = HarnessConfig
    packages["verifiers.v1.runtimes"].ProgramResult = ProgramResult
    packages["verifiers.v1.runtimes"].Runtime = type("Runtime", (), {})
    packages["verifiers.v1.trace"].Trace = type("Trace", (), {})
    sys.modules.update(packages)


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
install_verifiers_stubs()
harness_module = importlib.import_module("codewhale_harness.harness")

CodewhaleHarness = harness_module.CodewhaleHarness
CodewhaleHarnessConfig = harness_module.CodewhaleHarnessConfig
ProgramResult = sys.modules["verifiers.v1.runtimes"].ProgramResult


def event(event_type: str, **fields) -> str:
    return json.dumps(
        {
            "schema": "codewhale.exec-stream",
            "schema_version": 1,
            "type": event_type,
            **fields,
        }
    )


def successful_stream(model: str = "test/model", sandbox: str = "workspace-write") -> str:
    sha = "sha256:" + "a" * 64
    return "\n".join(
        [
            event("content", content="sensitive model output"),
            event(
                "metadata",
                meta={
                    "receipt_kind": "terminal",
                    "provider": "openai",
                    "model": model,
                    "approval_posture": "auto_tools",
                    "sandbox_posture": sandbox,
                    "binary_sha256": sha,
                    "prompt_sha256": sha,
                    "status": "completed",
                    "termination_reason": "resolved",
                    "workspace": "/private/runtime/path",
                    "session_id": "private-session-id",
                },
            ),
            event("done"),
        ]
    )


class FakeRuntime:
    def __init__(self, runtime_type="subprocess", result=None):
        self.type = runtime_type
        self.result = result or ProgramResult(0, successful_stream(), "")
        self.runs = []
        self.programs = []
        self.writes = []

    async def run(self, argv, env):
        self.runs.append((argv, env))
        if argv[-1:] == ["--version"]:
            return ProgramResult(0, "codewhale 0.9.1 (candidate)", "")
        return ProgramResult(0, "", "")

    async def run_program(self, argv, env):
        self.programs.append((argv, env))
        return self.result

    async def write(self, path, data):
        self.writes.append((path, data))


def trace(prompt="Do the task", system_prompt="Use evidence"):
    return SimpleNamespace(
        id="0123456789abcdef",
        task=SimpleNamespace(
            data=SimpleNamespace(prompt=prompt, system_prompt=system_prompt)
        ),
        info={},
    )


class HarnessTests(unittest.IsolatedAsyncioTestCase):
    async def test_preinstalled_candidate_requires_exact_version(self):
        config = CodewhaleHarnessConfig(
            version="0.9.1", binary_path="/candidate/codewhale"
        )
        runtime = FakeRuntime()

        await CodewhaleHarness(config).setup(runtime)

        self.assertEqual(runtime.runs, [(["/candidate/codewhale", "--version"], {})])

    async def test_launch_isolates_config_routes_interception_and_keeps_safe_receipt(self):
        config = CodewhaleHarnessConfig(
            version="0.9.1",
            binary_path="/candidate/codewhale",
            max_turns=17,
            disabled_tools=["web_run"],
            env={"CALLER_SETTING": "kept", "OPENAI_API_KEY": "must-not-win"},
        )
        runtime = FakeRuntime()
        rollout = trace()
        ctx = SimpleNamespace(model="test/model")

        await CodewhaleHarness(config).launch(
            ctx,
            rollout,
            runtime,
            "http://127.0.0.1:9000/v1/",
            "vf-session-secret",
            {"grader": "http://127.0.0.1:8123/mcp"},
        )

        self.assertEqual(len(runtime.programs), 1)
        argv, env = runtime.programs[0]
        self.assertEqual(argv[0], "/candidate/codewhale")
        self.assertIn("--no-project-config", argv)
        self.assertIn("--skip-onboarding", argv)
        self.assertEqual(argv[argv.index("--sandbox") + 1], "workspace-write")
        self.assertEqual(argv[argv.index("--max-turns") + 1], "17")
        self.assertEqual(argv[argv.index("--disallowed-tools") + 1], "web_run")
        self.assertEqual(argv[argv.index("--append-system-prompt") + 1], "Use evidence")
        self.assertNotIn("vf-session-secret", argv)
        self.assertEqual(env["OPENAI_API_KEY"], "vf-session-secret")
        self.assertEqual(env["OPENAI_BASE_URL"], "http://127.0.0.1:9000/v1")
        self.assertEqual(env["CODEWHALE_PROVIDER"], "openai")
        self.assertEqual(env["CODEWHALE_MODEL"], "test/model")
        self.assertEqual(env["CODEWHALE_TELEMETRY"], "false")
        self.assertEqual(env["CODEWHALE_ALLOW_INSECURE_HTTP"], "1")
        self.assertEqual(env["CALLER_SETTING"], "kept")
        self.assertEqual(len(runtime.writes), 1)
        mcp_path, mcp_bytes = runtime.writes[0]
        self.assertEqual(mcp_path, env["CODEWHALE_MCP_CONFIG"])
        self.assertTrue(mcp_path.startswith(".vf-codewhale/"))
        self.assertNotIn(rollout.id, mcp_path)
        self.assertEqual(
            json.loads(mcp_bytes),
            {"servers": {"grader": {"url": "http://127.0.0.1:8123/mcp"}}},
        )

        receipt = rollout.info["codewhale"]
        self.assertEqual(receipt["events"], {"content": 1, "done": 1, "metadata": 1})
        self.assertNotIn("sensitive model output", json.dumps(receipt))
        self.assertNotIn("private-session-id", json.dumps(receipt))
        self.assertNotIn("/private/runtime/path", json.dumps(receipt))
        self.assertNotIn("vf-session-secret", json.dumps(receipt))

    async def test_isolated_runtime_uses_external_sandbox_without_elevation_flag(self):
        config = CodewhaleHarnessConfig(binary_path="/candidate/codewhale")
        runtime = FakeRuntime(
            runtime_type="prime",
            result=ProgramResult(
                0, successful_stream(sandbox="external-sandbox"), ""
            ),
        )

        await CodewhaleHarness(config).launch(
            SimpleNamespace(model="test/model"),
            trace(),
            runtime,
            "https://intercept.example/v1",
            "secret",
            {},
        )

        argv, env = runtime.programs[0]
        self.assertEqual(argv[argv.index("--sandbox") + 1], "external-sandbox")
        self.assertNotIn("--allow-sandbox-elevation", argv)
        self.assertNotIn("--max-turns", argv)
        self.assertNotIn("CODEWHALE_ALLOW_INSECURE_HTTP", env)

    async def test_success_without_exact_terminal_receipt_fails_closed(self):
        runtime = FakeRuntime(result=ProgramResult(0, event("content", content="ok"), ""))
        config = CodewhaleHarnessConfig(binary_path="/candidate/codewhale")

        with self.assertRaisesRegex(RuntimeError, "omitted terminal metadata"):
            await CodewhaleHarness(config).launch(
                SimpleNamespace(model="test/model"),
                trace(),
                runtime,
                "https://intercept.example/v1",
                "secret-not-in-error",
                {},
            )

    async def test_non_resolved_terminal_receipt_fails_closed(self):
        stdout = successful_stream().replace('"resolved"', '"approval_required"')
        runtime = FakeRuntime(result=ProgramResult(0, stdout, ""))
        config = CodewhaleHarnessConfig(binary_path="/candidate/codewhale")

        with self.assertRaisesRegex(RuntimeError, "was not resolved"):
            await CodewhaleHarness(config).launch(
                SimpleNamespace(model="test/model"),
                trace(),
                runtime,
                "https://intercept.example/v1",
                "secret-not-in-error",
                {},
            )

    async def test_oversized_terminal_receipt_fails_closed_without_copying_content(self):
        stdout = successful_stream().replace("test/model", "x" * 513)
        runtime = FakeRuntime(result=ProgramResult(0, stdout, ""))
        config = CodewhaleHarnessConfig(binary_path="/candidate/codewhale")

        with self.assertRaisesRegex(RuntimeError, "exceeded its string bound"):
            await CodewhaleHarness(config).launch(
                SimpleNamespace(model="x" * 513),
                trace(),
                runtime,
                "https://intercept.example/v1",
                "secret-not-in-error",
                {},
            )

    def test_exact_version_rejects_prerelease_and_longer_tokens(self):
        has_version = harness_module._has_version

        self.assertTrue(has_version("codewhale 0.9.1 (abc123)", "0.9.1"))
        self.assertFalse(has_version("codewhale 0.9.1-rc.1", "0.9.1"))
        self.assertFalse(has_version("codewhale 0.9.10", "0.9.1"))

    async def test_invalid_config_rejects_shell_and_tool_injection(self):
        for config in [
            CodewhaleHarnessConfig(version="0.9.1; touch /tmp/pwned"),
            CodewhaleHarnessConfig(disabled_tools=["read_file,exec_shell"]),
            CodewhaleHarnessConfig(max_turns=0),
        ]:
            with self.assertRaises(ValueError):
                await CodewhaleHarness(config).setup(FakeRuntime())

    async def test_install_is_pinned_and_checksum_verified(self):
        config = CodewhaleHarnessConfig(version="0.9.1")
        runtime = FakeRuntime()

        await CodewhaleHarness(config).setup(runtime)

        self.assertEqual(len(runtime.runs), 1)
        argv, env = runtime.runs[0]
        self.assertEqual(argv[:2], ["sh", "-c"])
        self.assertEqual(env, {})
        command = argv[2]
        self.assertIn("releases/download", command)
        self.assertIn("codewhale-artifacts-sha256.txt", command)
        self.assertIn("sha256sum", command)
        self.assertIn("flock", command)
        self.assertIn("util-linux", command)
        for staged in ('"$install_dir/bin/$target.tmp.$$"', '"$install_dir/bin/.sha256.tmp"', '"$install_dir/bin/.version.tmp"'):
            self.assertLess(command.index(f"sync {staged}"), command.index(f"mv -f {staged}"))
        self.assertIn('sync "$install_dir/bin"\n', command)


if __name__ == "__main__":
    unittest.main()
