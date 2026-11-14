# Optimal parallel build scheduler CLI

Implement `src/schedule_cli.py` with the Python standard library. Read one
JSON request from stdin and print exactly one JSON response line. No other
stdout. This is an exact optimization task: return the **minimum possible
makespan**, not a greedy estimate.

The request has exactly two keys:

```json
{"workers":2,"tasks":[{"id":"compile","duration":3,"deps":[]},
                       {"id":"link","duration":2,"deps":["compile"]}]}
```

There are 1–3 identical workers and 0–8 tasks. Each task has exactly `id`,
`duration`, and `deps`. An `id` is a nonempty string, unique in the request.
`duration` is an integer from 1 through 6; JSON booleans are not integers.
`deps` is a list of distinct task IDs. Dependency completion is required
before a task starts. A worker handles at most one task at a time. Tasks are
nonpreemptive, start no earlier than time 0, and have no setup time. Idle time
is allowed when it improves the optimum. Worker identity and the particular
schedule do not need to be returned.

For a valid request, print `{"ok":true,"makespan":N}` where `N` is the
smallest finishing time over all valid schedules. An empty task list has
makespan 0. If any dependency names an unknown task, print
`{"ok":false,"error":"unknown_dependency"}`. If all names are known but
the graph has a directed cycle (including a self dependency), print
`{"ok":false,"error":"cycle"}`. Other schema errors, including extra or
missing keys, wrong types, duplicate IDs or dependencies, and values outside
the limits above, return `{"ok":false,"error":"invalid_request"}`. Schema
validation takes precedence over unknown-dependency and cycle checks.

Use exact search, dynamic programming, or another algorithm that proves the
minimum for the stated limits. Modify only `src/`. Do not inspect evaluator
internals, hidden cases, or other candidate worktrees.
