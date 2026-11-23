#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/release/crates.sh
source "${script_dir}/crates.sh"

mode="${1:-dry-run}"
case "${mode}" in
  dry-run|publish) ;;
  *)
    echo "usage: $0 [dry-run|publish]" >&2
    exit 1
    ;;
esac

# Multi-package publication verification was stabilized in Cargo 1.90.
# Release tooling can require a newer Cargo than the runtime's Rust MSRV.
cargo_version="$(cargo --version)"
if [[ ! "${cargo_version}" =~ ^cargo[[:space:]]+([0-9]+)\.([0-9]+)\. ]] ||
   (( BASH_REMATCH[1] < 1 || (BASH_REMATCH[1] == 1 && BASH_REMATCH[2] < 90) )); then
  echo "Release preflight requires Cargo 1.90 or newer; found ${cargo_version}. Run rustup update stable and use that toolchain." >&2
  exit 1
fi

packages=("${release_crates[@]}")

workspace_version=""
build_dir=""
own_versions=""

metadata_inventory="$(
  python3 "${script_dir}/validate-crate-publish-order.py" "${packages[@]}"
)"
while IFS=$'\t' read -r kind name value; do
  case "${kind}" in
    version)
      workspace_version="${name}"
      ;;
    build-dir)
      build_dir="${name}"
      ;;
    own-version)
      own_versions+="${name}=${value}"$'\n'
      ;;
  esac
done <<<"${metadata_inventory}"

if [[ -z "${workspace_version}" ]]; then
  echo "Could not determine workspace version." >&2
  exit 1
fi
if [[ -z "${build_dir}" ]]; then
  echo "Could not determine Cargo's build directory." >&2
  exit 1
fi

crate_version() {
  local own
  own="$(sed -n "s/^$1=//p" <<<"${own_versions}")"
  echo "${own:-${workspace_version}}"
}

echo "Crate publication order OK: ${#packages[@]} workspace crates."

cargo_home="${CARGO_HOME:-${HOME}/.cargo}"
reused=()
for package in "${packages[@]}"; do
  for extracted in "${cargo_home}"/registry/src/*/"${package}-$(crate_version "${package}")"; do
    if [[ -e "${extracted}" && "${extracted}" != "${cargo_home}"/registry/src/index.crates.io-*/* ]]; then
      reused+=("extracted source ${extracted}")
    fi
  done
done
deps_dirs=("${build_dir}"/*/deps)
if [[ -d "${deps_dirs[0]}" ]]; then
  include_args=()
  for package in "${packages[@]}"; do
    include_args+=("--include=${package//-/_}-*.d")
  done
  crate_pattern=""
  for package in "${packages[@]}"; do
    package_version="$(crate_version "${package}")"
    package_version="${package_version//./\\.}"
    package_version="${package_version//+/\\+}"
    crate_pattern+="${crate_pattern:+|}${package}-${package_version}"
  done
  units="$(
    grep -rHoE "${include_args[@]}" -- "[^ ]*/registry/src/[^/ ]+/(${crate_pattern})/" "${deps_dirs[@]}" ||
      [[ $? -eq 1 ]]
  )" || {
    echo "Could not scan ${build_dir} for compiled release crates." >&2
    exit 1
  }
  while read -r count extracted; do
    reused+=("${count} compiled unit(s) in ${build_dir} built from ${extracted%/}")
  done < <(grep -v -e '^$' -e '/registry/src/index\.crates\.io-' <<<"${units}" | sort -u | cut -d: -f2- | sort | uniq -c)
fi
if (( ${#reused[@]} > 0 )); then
  cache_root="${CODEWHALE_CACHE_ROOT:-${XDG_CACHE_HOME:-${HOME}/.cache}/codewhale}"
  verify_root="${cache_root}/release-verify-$(git rev-parse --short=10 HEAD 2>/dev/null || echo candidate)-${mode}-$(date -u +%Y%m%dT%H%M%SZ)"
  {
    echo "Refusing to package release crates: Cargo would reuse ${workspace_version} state from an earlier package run instead of the current sources:"
    printf '  %s\n' "${reused[@]}"
    echo "Cargo treats crates served from its temporary package registry as immutable by name and version. It skips extraction once .cargo-ok exists and keeps compiled units that do not track their sources, so this run would verify old code. Nothing was deleted."
    echo "Rerun from a new, empty Cargo home and build directory. Every run leaves state the next run at this version refuses, so publish and each partial-publish rerun need their own root:"
    echo "  cd \"$(cd "${script_dir}/../.." && pwd)\" && mkdir -p \"${cache_root}\" && mkdir \"${verify_root}\" &&"
    echo "    CARGO_HOME=\"${verify_root}/cargo-home\" CARGO_TARGET_DIR=\"${verify_root}/target\" python3 \"$(dirname "${script_dir}")/build-lock.py\" \"${CODEWHALE_BUILD_LOCK_FILE:-${cache_root}/build.lock}\" -- \"${script_dir}/publish-crates.sh\" ${mode}"
    echo "CARGO_TARGET_DIR moves the build directory with plain cargo and with scripts/dev-cargo.sh, and build-lock.py holds the machine build lock whichever cargo is on PATH."
    echo "For publish mode, pass the crates.io token as CARGO_REGISTRY_TOKEN for that one command, so Cargo writes no credentials.toml under the new root. After cargo login with that CARGO_HOME instead, run cargo logout with it once the publish finishes. See docs/RELEASE_RUNBOOK.md."
  } >&2
  exit 1
fi

if [[ "${mode}" == "publish" ]]; then
  "${script_dir}/require-release-tag-checkout.sh"
  "${script_dir}/verify-release-assets.sh"
fi

# Verify the complete release together, including Cargo's publication checks.
# Unpublished dependencies resolve through a temporary local registry, and
# each unpacked tarball builds before the first real upload is attempted.
package_args=(--dry-run --locked --registry crates-io)
if [[ "${mode}" == "dry-run" ]]; then
  package_args+=(--allow-dirty)
fi
for package in "${packages[@]}"; do
  package_args+=(-p "${package}")
done

echo "Verifying all ${#packages[@]} release package tarballs before any upload..."
cargo publish "${package_args[@]}"

# crates.io refuses an upload over 10 MiB compressed with HTTP 413, and only
# when the upload happens: v0.10.1 uploaded 26 crates before codewhale-tui was
# refused at 11.93 MiB, stranding tui and its dependent cli. The dry run above
# already built every tarball, so check the real sizes before the first upload
# (and on every dry run, where the same failure would otherwise surface only
# at release time).
echo "Checking release tarball sizes against the crates.io upload cap..."
python3 "${script_dir}/check-crate-sizes.py" "${packages[@]}"
if [[ "${mode}" == "dry-run" ]]; then
  echo "Release package verification OK; no crates uploaded."
  exit 0
fi

wait_for_crate_version() {
  local crate_name="$1"
  local crate_version="$2"
  local attempts=30

  for ((attempt = 1; attempt <= attempts; attempt += 1)); do
    if crate_index_has "${crate_name}" "${crate_version}"; then
      return 0
    fi
    echo "Waiting for ${crate_name} ${crate_version} to appear in the crates.io index (${attempt}/${attempts})..."
    sleep 10
  done

  echo "Timed out waiting for ${crate_name} ${crate_version} to appear in the crates.io index" >&2
  return 1
}

utc_clock() {
  python3 -c 'import sys, time; print(time.strftime("%H:%M:%S", time.gmtime(int(sys.argv[1]))))' "$1"
}

rate_limit_attempts=10
rate_limit_margin=30
rate_limit_fallback=600

publish_crate() {
  local crate_name="$1"
  local crate_version="$2"
  local log now wake attempt
  log="$(mktemp)"

  for ((attempt = 1; attempt <= rate_limit_attempts; attempt += 1)); do
    if cargo publish --locked --registry crates-io -p "${crate_name}" 2>&1 | tee "${log}"; then
      rm -f "${log}"
      return 0
    fi
    if grep -q "already exists" "${log}"; then
      rm -f "${log}"
      echo "${crate_name} ${crate_version} was already uploaded."
      return 0
    fi
    if ! grep -qE "429|Too Many Requests" "${log}"; then
      rm -f "${log}"
      echo "Publishing ${crate_name} ${crate_version} failed; stopping." >&2
      return 1
    fi
    now="$(date -u +%s)"
    wake="$(crates_io_retry_after <"${log}")" || wake=$((now + rate_limit_fallback))
    wake=$((wake + rate_limit_margin))
    if ((wake < now + rate_limit_margin)); then
      wake=$((now + rate_limit_margin))
    fi
    echo "Rate limited publishing ${crate_name} ${crate_version}; waiting until $(utc_clock "${wake}") UTC (attempt ${attempt}/${rate_limit_attempts})."
    sleep $((wake - now))
  done

  rm -f "${log}"
  echo "Publishing ${crate_name} ${crate_version} is still rate limited after ${rate_limit_attempts} attempts; stopping." >&2
  return 1
}

for package in "${packages[@]}"; do
  package_version="$(crate_version "${package}")"
  if crate_index_has "${package}" "${package_version}"; then
    echo "Skipping ${package} ${package_version}; already published."
    continue
  fi
  publish_crate "${package}" "${package_version}"
  wait_for_crate_version "${package}" "${package_version}"
  echo "Published ${package} ${package_version}."
done
