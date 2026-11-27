#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
checker="${repo_root}/scripts/check-behaviour-line.py"
python_bin="${PYTHON:-python3}"
fixture="$(mktemp -d)"
trap 'rm -rf "${fixture}"' EXIT

git -C "${fixture}" init -q
git -C "${fixture}" config user.name "Behaviour Test"
git -C "${fixture}" config user.email "behaviour-test@example.com"

printf 'seed\n' >"${fixture}/seed.txt"
git -C "${fixture}" add seed.txt
git -C "${fixture}" commit -qm "chore: seed"
base="$(git -C "${fixture}" rev-parse HEAD)"

# A fix without a statement fails.
printf 'a\n' >"${fixture}/a.txt"
git -C "${fixture}" add a.txt
git -C "${fixture}" commit -qm "fix(engine): drop a default"

if (cd "${fixture}" && "${python_bin}" "${checker}" "${base}" HEAD >missing.out 2>&1); then
  echo "missing behaviour statement unexpectedly passed" >&2
  exit 1
fi
grep -Fq '1 of 1 behaviour-changing commit(s)' "${fixture}/missing.out"
grep -Fq 'fix(engine): drop a default' "${fixture}/missing.out"

# --warn-only reports the same finding and exits 0.
(cd "${fixture}" && "${python_bin}" "${checker}" --warn-only "${base}" HEAD >warn.out 2>&1)
grep -Fq '1 of 1 behaviour-changing commit(s)' "${fixture}/warn.out"

# A Behaviour: line passes.
printf 'b\n' >"${fixture}/b.txt"
git -C "${fixture}" add b.txt
git -C "${fixture}" commit -qm "fix(engine): restore the default" \
  -m "Behaviour: retry budget 2 -> 4 attempts"

# A No-behaviour-change: line passes.
printf 'c\n' >"${fixture}/c.txt"
git -C "${fixture}" add c.txt
git -C "${fixture}" commit -qm "feat(engine): reorganise the queue" \
  -m "No-behaviour-change: no default, limit or rendered block changed"

# A docs commit needs neither.
printf 'd\n' >"${fixture}/d.txt"
git -C "${fixture}" add d.txt
git -C "${fixture}" commit -qm "docs: explain the queue"

# The range with one missing commit still fails...
if (cd "${fixture}" && "${python_bin}" "${checker}" "${base}" HEAD >all.out 2>&1); then
  echo "range with a missing statement unexpectedly passed" >&2
  exit 1
fi
grep -Fq '1 of 3 behaviour-changing commit(s)' "${fixture}/all.out"

# ...and the clean range passes.
base2="$(git -C "${fixture}" rev-parse HEAD~3)"
(cd "${fixture}" && "${python_bin}" "${checker}" "${base2}" HEAD >clean.out 2>&1)
grep -Fq '2 behaviour-changing commit(s) carry a statement' "${fixture}/clean.out"

echo "check-behaviour-line tests passed"
