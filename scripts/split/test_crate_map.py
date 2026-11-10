#!/usr/bin/env python3
from __future__ import annotations

import collections
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import crate_map  # noqa: E402

CFG = {
    "ceiling": 100,
    "target": 50,
    "crates": {
        "engine": {"layer": 1, "units": ["lower", "tools/*"]},
        "engine-loop": {"layer": 2, "units": ["loop"]},
        "tui": {"layer": 3, "units": ["ui"]},
    },
}


def graph(edges: dict[tuple[str, str], int], extra_units: tuple[str, ...] = ()):
    sizes = collections.Counter({u: 10 for u in ("lower", "tools/<self>", "tools/spec", "loop", "ui", *extra_units)})
    items = collections.defaultdict(collections.Counter)
    for pair, n in edges.items():
        items[pair]["Item"] = n
    return sizes, collections.Counter(edges), items


class EvaluateTest(unittest.TestCase):
    def test_downward_only_map_is_clean(self):
        r = crate_map.evaluate(CFG, *graph({("loop", "lower"): 3, ("ui", "loop"): 2}))
        self.assertEqual(crate_map.structural_problems(r), [])
        self.assertEqual(r["upward_pairs"], {})
        self.assertEqual(crate_map.summary(r)["cycles"], 0)

    def test_unassigned_unit_and_unmatched_pattern_fail(self):
        cfg = {**CFG, "crates": {**CFG["crates"], "tui": {"layer": 3, "units": ["ui", "gone"]}}}
        r = crate_map.evaluate(cfg, *graph({}, extra_units=("orphan",)))
        problems = crate_map.structural_problems(r)
        self.assertIn("pattern matched nothing: tui: gone", problems)
        self.assertIn("unassigned unit: orphan (10 lines)", problems)

    def test_upward_pair_is_keyed_by_crates_and_units(self):
        r = crate_map.evaluate(CFG, *graph({("tools/spec", "loop"): 4, ("loop", "lower"): 9}))
        self.assertEqual(r["upward_pairs"], {"engine->engine-loop|tools/spec->loop": 4})
        self.assertEqual(crate_map.summary(r)["cycles"], 1)


class RatchetTest(unittest.TestCase):
    def setUp(self):
        self.r = crate_map.evaluate(CFG, *graph({("tools/spec", "loop"): 4}))
        self.baseline = crate_map.baseline_document("map.toml", self.r)

    def test_unchanged_references_hold(self):
        self.assertEqual(crate_map.ratchet_problems(self.r, self.baseline), [])

    def test_forbidden_edge_to_ui_fails_as_new_pair(self):
        r = crate_map.evaluate(CFG, *graph({("tools/spec", "loop"): 4, ("lower", "ui"): 1}))
        problems = crate_map.ratchet_problems(r, self.baseline)
        self.assertIn("  engine->tui|lower->ui: new upward reference pair (1 refs)", problems)

    def test_rise_fails(self):
        r = crate_map.evaluate(CFG, *graph({("tools/spec", "loop"): 5}))
        self.assertIn("  engine->engine-loop|tools/spec->loop: 4 -> 5", crate_map.ratchet_problems(r, self.baseline))

    def test_drop_without_lowering_fails(self):
        r = crate_map.evaluate(CFG, *graph({}))
        problems = crate_map.ratchet_problems(r, self.baseline)
        self.assertIn("  engine->engine-loop|tools/spec->loop: 4 -> 0", problems)

    def test_baseline_raised_against_ref_fails(self):
        previous = {"upward": {"engine->engine-loop|tools/spec->loop": 3}}
        problems = crate_map.ratchet_problems(self.r, self.baseline, previous, "base")
        self.assertTrue(problems[0].startswith("the baseline was raised relative to base"))


if __name__ == "__main__":
    unittest.main()
