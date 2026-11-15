import functools
import itertools
import json
import random
import subprocess
import sys


class GateInputError(Exception):
    pass


def expected(request):
    if not isinstance(request, dict) or set(request) != {"workers", "tasks"}:
        return {"ok": False, "error": "invalid_request"}
    workers = request["workers"]
    tasks = request["tasks"]
    if type(workers) is not int or not 1 <= workers <= 3 or not isinstance(tasks, list) or len(tasks) > 8:
        return {"ok": False, "error": "invalid_request"}
    ids = []
    for task in tasks:
        if not isinstance(task, dict) or set(task) != {"id", "duration", "deps"}:
            return {"ok": False, "error": "invalid_request"}
        name, duration, dependencies = task["id"], task["duration"], task["deps"]
        if (not isinstance(name, str) or not name or name in ids
                or type(duration) is not int or not 1 <= duration <= 6
                or not isinstance(dependencies, list)
                or any(not isinstance(dep, str) for dep in dependencies)
                or len(dependencies) != len(set(dependencies))):
            return {"ok": False, "error": "invalid_request"}
        ids.append(name)
    if any(dep not in ids for task in tasks for dep in task["deps"]):
        return {"ok": False, "error": "unknown_dependency"}
    dep_masks = []
    for task in tasks:
        dep_masks.append(sum(1 << ids.index(dep) for dep in task["deps"]))
    remaining = set(range(len(tasks)))
    done = 0
    while remaining:
        ready = [i for i in remaining if dep_masks[i] & ~done == 0]
        if not ready:
            return {"ok": False, "error": "cycle"}
        for i in ready:
            remaining.remove(i)
            done |= 1 << i
    durations = tuple(task["duration"] for task in tasks)
    full = (1 << len(tasks)) - 1

    @functools.lru_cache(None)
    def optimum(completed, running):
        if completed == full and not running:
            return 0
        active_ids = {i for i, _ in running}
        available = [i for i in range(len(tasks)) if not (completed >> i & 1)
                     and i not in active_ids and dep_masks[i] & ~completed == 0]
        slots = workers - len(running)
        best = float("inf")
        for count in range(min(slots, len(available)) + 1):
            for additions in itertools.combinations(available, count):
                if not running and not additions:
                    continue
                active = running + tuple((i, durations[i]) for i in additions)
                elapsed = min(time_left for _, time_left in active)
                next_completed = completed
                next_running = []
                for i, time_left in active:
                    if time_left == elapsed:
                        next_completed |= 1 << i
                    else:
                        next_running.append((i, time_left - elapsed))
                candidate = elapsed + optimum(next_completed, tuple(sorted(next_running)))
                best = min(best, candidate)
        return best

    return {"ok": True, "makespan": optimum(0, ())}


def valid_cases():
    cases = [
        {"workers": 2, "tasks": []},
        {"workers": 1, "tasks": [{"id": "a", "duration": 3, "deps": []}]},
        {"workers": 2, "tasks": [{"id": "a", "duration": 3, "deps": []},
                                   {"id": "b", "duration": 2, "deps": []}]},
        {"workers": 2, "tasks": [{"id": "a", "duration": 3, "deps": []},
                                   {"id": "b", "duration": 2, "deps": ["a"]}]},
        {"workers": 2, "tasks": [{"id": "a", "duration": 6, "deps": []},
                                   {"id": "b", "duration": 2, "deps": []},
                                   {"id": "c", "duration": 2, "deps": ["b"]},
                                   {"id": "d", "duration": 2, "deps": ["c"]},
                                   {"id": "e", "duration": 2, "deps": ["d"]}]},
        {"workers": 3, "tasks": [{"id": "a", "duration": 5, "deps": []},
                                   {"id": "b", "duration": 4, "deps": []},
                                   {"id": "c", "duration": 3, "deps": []},
                                   {"id": "d", "duration": 2, "deps": ["a", "b"]}]},
    ]
    rng = random.Random(7250)
    for number in range(20):
        length = 5 + number % 4
        workers = 1 + number % 3
        tasks = []
        for i in range(length):
            deps = [f"t{j}" for j in range(i) if rng.random() < 0.27]
            tasks.append({"id": f"t{i}", "duration": rng.randint(1, 6), "deps": deps})
        cases.append({"workers": workers, "tasks": tasks})
    return cases


CASES = [(request, expected(request)) for request in valid_cases()]
CASES += [
    ({"workers": 0, "tasks": []}, {"ok": False, "error": "invalid_request"}),
    ({"workers": True, "tasks": []}, {"ok": False, "error": "invalid_request"}),
    ({"workers": 2, "tasks": [{"id": "a", "duration": 1, "deps": ["missing"]}]},
     {"ok": False, "error": "unknown_dependency"}),
    ({"workers": 2, "tasks": [{"id": "a", "duration": 1, "deps": ["b"]},
                               {"id": "b", "duration": 1, "deps": ["a"]}]},
     {"ok": False, "error": "cycle"}),
    ({"workers": 2, "tasks": [{"id": "a", "duration": 1, "deps": []},
                               {"id": "a", "duration": 2, "deps": []}]},
     {"ok": False, "error": "invalid_request"}),
    ({"workers": 2, "tasks": [], "extra": 1}, {"ok": False, "error": "invalid_request"}),
]


def main():
    failures = []
    for index, (request, oracle) in enumerate(CASES, 1):
        completed = subprocess.run([sys.executable, "src/schedule_cli.py"],
                                   input=json.dumps(request), text=True,
                                   capture_output=True, timeout=5, check=False)
        try:
            actual = json.loads(completed.stdout)
        except ValueError:
            actual = completed.stdout.strip()
        if completed.returncode != 0 or actual != oracle:
            failures.append((index, request, oracle, actual, completed.returncode, completed.stderr[-200:]))
    print(f"Ran {len(CASES)} tests in process-level optimal-scheduler gate")
    for index, request, oracle, actual, code, stderr in failures:
        print(f"FAIL: case {index}: request={request!r}, expected={oracle!r}, got={actual!r}, exit={code}, stderr={stderr!r}")
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
