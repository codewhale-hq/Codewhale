#!/usr/bin/env bash
set -euo pipefail

: "${ARTIFACTS_DIR:?}" "${GH_TOKEN:?}" "${APP_ID:?}" "${GITHUB_REPOSITORY:?}" "${RUN_URL:?}" "${TRUSTED_DIR:?}"

git config user.name 'codewhale-agent[bot]'
git config user.email "${APP_ID}+codewhale-agent[bot]@users.noreply.github.com"
git fetch --quiet origin main
base=$(git rev-parse origin/main)

published=0
for lane in derived copy-freshness link-health docs-drift; do
  dir="${ARTIFACTS_DIR}/web-maintainer-${lane}"
  patch="${dir}/change.patch"
  if [ ! -s "$patch" ]; then
    echo "${lane}: no patch to publish"
    continue
  fi
  rules="${TRUSTED_DIR}/allowlist.txt"
  [ "$lane" = derived ] && rules="${TRUSTED_DIR}/derived.txt"

  branch="web-maintainer/${lane}"
  git checkout --quiet --detach "$base"
  git reset --quiet --hard "$base"
  git clean --quiet -fdx -e node_modules

  if [ "$(wc -c < "$patch")" -gt 262144 ]; then
    echo "::error::${lane}: patch exceeds 256 KiB; not published"
    continue
  fi
  if ! git apply --index --whitespace=nowarn "$patch"; then
    echo "::error::${lane}: patch does not apply to current main; not published"
    continue
  fi
  if [ -n "$(git diff --cached --summary)" ]; then
    echo "::error::${lane}: patch creates, deletes, renames or changes modes; not published"
    git reset --quiet --hard "$base"
    continue
  fi
  if ! git diff --cached --name-status | node "${TRUSTED_DIR}/check-allowlist.mjs" "$rules" "$lane"; then
    echo "::error::${lane}: patch is outside the allowlist; not published"
    git reset --quiet --hard "$base"
    continue
  fi

  title="chore(web): ${lane} maintenance"
  git commit --quiet -m "$title" \
    -m "Proposed by .github/workflows/web-maintainer.yml (lane: ${lane}). Reviewed by a person before merge."
  git push --force-with-lease="refs/heads/${branch}" \
    "https://x-access-token:${GH_TOKEN}@github.com/${GITHUB_REPOSITORY}.git" "HEAD:refs/heads/${branch}"

  {
    echo "Proposed by the web maintainer, lane \`${lane}\`. Nothing merges automatically; review it like any other change."
    echo
    if [ "$lane" = derived ]; then
      echo "Deterministic: regenerated files only. No model wrote these changes."
    else
      echo "A model (DeepSeek through \`codewhale exec\`) edited existing website copy inside this lane's path allowlist. The job checked every changed path against \`web/maintainer/allowlist.txt\`, then ran \`npm run check\` and \`vitest\` in \`web/\` before this pull request was opened."
    fi
    echo
    echo "Files changed ($(git diff --name-only "$base" HEAD | wc -l | tr -d ' ')):"
    git diff --name-only "$base" HEAD | sed 's/^/- `/; s/$/`/'
    echo
    echo '```text'
    git diff --stat "$base" HEAD
    echo '```'
    if [ "$lane" != derived ]; then
      echo
      echo "Model-written copy. Read the full diff, not only the diffstat, before approving."
    fi
    echo
    echo "Run: ${RUN_URL}"
    echo
    echo "To stop these proposals, unset the repository variable \`WEB_MAINTAINER_ENABLED\`. See \`web/maintainer/README.md\`."
  } > "${RUNNER_TEMP:-/tmp}/pr-body-${lane}.md"

  existing=$(gh pr list --head "$branch" --state open --json number --jq '.[0].number // empty')
  if [ -n "$existing" ]; then
    gh pr edit "$existing" --title "$title" --body-file "${RUNNER_TEMP:-/tmp}/pr-body-${lane}.md"
    echo "${lane}: updated #${existing}" | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
  else
    url=$(gh pr create --base main --head "$branch" --title "$title" --body-file "${RUNNER_TEMP:-/tmp}/pr-body-${lane}.md")
    echo "${lane}: opened ${url}" | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
  fi
  published=$((published + 1))
done

echo "published ${published} pull request(s)" | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
