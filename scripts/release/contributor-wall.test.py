#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "contributor_wall",
    Path(__file__).resolve().parents[1] / "contributor-wall.py",
)
WALL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WALL)


class ContributorWallTests(unittest.TestCase):
    def test_users_exist_accepts_not_found_entries(self):
        payload = {
            "data": {"u0": {"login": "OctoCat", "name": "Octo"}, "u1": None},
            "errors": [{"type": "NOT_FOUND", "path": ["u1"]}],
        }
        completed = subprocess.CompletedProcess(
            args=["gh"], returncode=1, stdout=json.dumps(payload), stderr="gh: not found"
        )
        with mock.patch.object(WALL.subprocess, "run", return_value=completed):
            found = WALL.users_exist({"octocat", "zzz-missing-handle"})
        self.assertEqual(found, {"octocat": {"login": "OctoCat", "name": "Octo"}})

    def test_users_exist_fails_loud_on_rate_limit_or_empty_response(self):
        rate_limited = subprocess.CompletedProcess(
            args=["gh"],
            returncode=1,
            stdout=json.dumps({"data": None, "errors": [{"type": "RATE_LIMITED"}]}),
            stderr="API rate limit exceeded",
        )
        with mock.patch.object(WALL.subprocess, "run", return_value=rate_limited):
            with self.assertRaises(RuntimeError):
                WALL.users_exist({"octocat"})

        empty = subprocess.CompletedProcess(
            args=["gh"], returncode=1, stdout="", stderr="HTTP 502"
        )
        with mock.patch.object(WALL.subprocess, "run", return_value=empty):
            with self.assertRaises(RuntimeError):
                WALL.users_exist({"octocat"})


if __name__ == "__main__":
    unittest.main()
