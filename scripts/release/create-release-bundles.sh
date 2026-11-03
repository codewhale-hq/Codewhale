#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 INPUT_ARTIFACT_DIR OUTPUT_BUNDLE_DIR" >&2
  exit 2
fi

artifact_dir="$1"
bundle_dir="$2"

# Archive metadata must be stable across recovery builds. The public workflow
# still refuses to replace existing release assets; reproducible packaging is a
# diagnostic and provenance aid, not permission to overwrite published bytes.
export TZ=UTC
if [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
  echo "SOURCE_DATE_EPOCH is required; set it to the tagged/source commit timestamp" >&2
  exit 1
fi
if ! [[ "${SOURCE_DATE_EPOCH}" =~ ^[0-9]+$ ]]; then
  echo "SOURCE_DATE_EPOCH must be an integer Unix timestamp, got: ${SOURCE_DATE_EPOCH}" >&2
  exit 1
fi

# Trim leading zeroes before the length and range checks, avoiding arithmetic
# overflow from malformed values. ZIP stores DOS timestamps, whose valid range
# is 1980-01-01T00:00:00Z through 2107-12-31T23:59:58Z.
source_date_epoch="${SOURCE_DATE_EPOCH#"${SOURCE_DATE_EPOCH%%[!0]*}"}"
source_date_epoch="${source_date_epoch:-0}"
if (( ${#source_date_epoch} > 10 )) ||
  (( 10#${source_date_epoch} < 315532800 || 10#${source_date_epoch} > 4354819198 )); then
  echo "SOURCE_DATE_EPOCH must be between 315532800 (1980-01-01T00:00:00Z) and 4354819198 (2107-12-31T23:59:58Z) for ZIP archives" >&2
  exit 1
fi
source_date_epoch="$((10#${source_date_epoch}))"

if date --version >/dev/null 2>&1; then
  archive_timestamp="$(date -u -d "@${source_date_epoch}" '+%Y%m%d%H%M.%S')"
else
  archive_timestamp="$(date -u -r "${source_date_epoch}" '+%Y%m%d%H%M.%S')"
fi

if [[ ! -d "${artifact_dir}" ]]; then
  echo "input artifact directory does not exist: ${artifact_dir}" >&2
  exit 1
fi
artifact_dir="$(cd "${artifact_dir}" && pwd)"

if [[ -e "${bundle_dir}" && ! -d "${bundle_dir}" ]]; then
  echo "output bundle path is not a directory: ${bundle_dir}" >&2
  exit 1
fi
if [[ -e "${bundle_dir}" && -n "$(find "${bundle_dir}" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
  echo "output bundle directory must be empty: ${bundle_dir}" >&2
  exit 1
fi
mkdir -p "${bundle_dir}"
bundle_dir="$(cd "${bundle_dir}" && pwd)"

manifest="${bundle_dir}/codewhale-bundles-sha256.txt"
: > "${manifest}"

# Windows archives must contain CRLF batch files regardless of the builder's
# working-tree line endings (`* text=auto` checks out LF on macOS/Linux).
write_crlf_file() {
  local src="$1"
  local dest="$2"
  if [[ ! -f "${src}" ]]; then
    echo "missing Windows launcher or install script: ${src}" >&2
    exit 1
  fi
  awk '{ sub(/\r$/, ""); printf "%s\r\n", $0 }' "${src}" > "${dest}"
}

bundle() {
  local platform="$1"
  local cli_src="$2"
  local shim_src="$3"
  local ext="$4"
  local variant="$5"

  local stem="codewhale-${platform}${variant:+-}${variant}"
  local cli_dst="codewhale"
  local shim_dst="codew"
  if [[ "${platform}" == windows-* ]]; then
    cli_dst="codewhale.exe"
    shim_dst="codew.exe"
  fi

  local cli_path="${artifact_dir}/${cli_src}/${cli_src}"
  local shim_path="${artifact_dir}/${shim_src}/${shim_src}"
  if [[ ! -f "${cli_path}" ]]; then
    echo "missing required release artifact for ${platform}: ${cli_path}" >&2
    exit 1
  fi
  if [[ ! -f "${shim_path}" ]]; then
    echo "missing required release artifact for ${platform}: ${shim_path}" >&2
    exit 1
  fi

  local stage_root
  stage_root="$(mktemp -d)"
  local stage_dir="${stage_root}/${stem}"
  mkdir -p "${stage_dir}"

  cp "${cli_path}" "${stage_dir}/${cli_dst}"
  cp "${shim_path}" "${stage_dir}/${shim_dst}"

  # Optional images are projected from the same qualified, hash-checked
  # catalog. Android and targets without a receipt remain CLI-only.
  local host_dir="${artifact_dir}/codewhale-compiled-hosts"
  if [[ -f "${host_dir}/codewhale-extension-hosts.json" ]]; then
    node - "${host_dir}" "${stage_dir}" "${platform}" <<'NODE'
const fs = require('node:fs');
const path = require('node:path');
const hosts = require('./npm/codewhale/scripts/compiled-hosts');
const [directory, stage, target] = process.argv.slice(2);
const catalog = hosts.parseCatalog(fs.readFileSync(path.join(directory, hosts.HOST_CATALOG), 'utf8'));
const host = catalog.hosts.find((entry) => entry.target === target);
if (host) {
  hosts.verifyDirectory(directory, { ...catalog, hosts: [host] });
  const ext = target.startsWith('windows-') ? '.exe' : '';
  for (const [asset, name] of [[host.asset, hosts.HOST_NAME + ext], [host.notices_asset, hosts.HOST_NAME + '.LICENSES.txt'], [host.source_asset, hosts.HOST_NAME + '.relink-source.tar.gz']]) fs.copyFileSync(path.join(directory, asset), path.join(stage, name));
  fs.writeFileSync(path.join(stage, hosts.HOST_NAME + '.release.json'), JSON.stringify(catalog, null, 2) + '\n');
  if (!ext) fs.chmodSync(path.join(stage, hosts.HOST_NAME), 0o755);
}
NODE
  fi

  # actions/upload-artifact intentionally normalizes downloaded files to 0644.
  # Restore the executable contract before constructing Unix archives.
  if [[ "${platform}" != windows-* ]]; then
    chmod 0755 \
      "${stage_dir}/${cli_dst}" \
      "${stage_dir}/${shim_dst}"
  fi

  # Regular and portable Windows zips ship the Terminal-aware launcher (#1854).
  # The GitHub flat asset `codewhale.bat` still targets the x64 release filename;
  # archives rename the binary to codewhale.exe, so they reuse the NSIS launcher.
  if [[ "${platform}" == windows-* ]]; then
    write_crlf_file \
      scripts/installer/codewhale.bat \
      "${stage_dir}/codewhale.bat"
  fi

  if [[ "${variant}" != "portable" ]]; then
    if [[ "${platform}" == windows-* ]]; then
      write_crlf_file \
        scripts/release/install.bat \
        "${stage_dir}/install.bat"
      write_crlf_file scripts/release/install.ps1 "${stage_dir}/install.ps1"
    else
      cp scripts/release/install.sh "${stage_dir}/"
      chmod +x "${stage_dir}/install.sh"
    fi
  fi

  # zip and tar both record mtimes; normalize every staged entry to the exact
  # source commit timestamp so identical inputs do not produce checksum drift.
  find "${stage_dir}" -exec touch -t "${archive_timestamp}" {} +

  local archive="${bundle_dir}/${stem}.${ext}"
  if [[ "${ext}" == "zip" ]]; then
    (cd "${stage_root}" && zip -Xqr "${archive}" "${stem}/")
  elif tar --version 2>/dev/null | grep -q 'GNU tar'; then
    tar \
      --sort=name \
      --mtime="@${source_date_epoch}" \
      --owner=0 \
      --group=0 \
      --numeric-owner \
      --format=ustar \
      -cf - \
      -C "${stage_root}" \
      "${stem}/" | gzip -n > "${archive}"
  else
    COPYFILE_DISABLE=1 tar -cf - -C "${stage_root}" "${stem}/" | gzip -n > "${archive}"
  fi

  local checksum
  checksum="$(sha256sum "${archive}" | awk '{print $1}')"
  printf '%s  %s\n' "${checksum}" "$(basename "${archive}")" >> "${manifest}"
  rm -rf "${stage_root}"
  echo "Created ${archive}"
}

bundle linux-x64 \
  codewhale-linux-x64 codew-linux-x64 tar.gz ""
bundle linux-arm64 \
  codewhale-linux-arm64 codew-linux-arm64 tar.gz ""
bundle android-arm64 \
  codewhale-android-arm64 codew-android-arm64 tar.gz ""
# The archive carries install.sh like the other Unix bundles, but on HarmonyOS
# the binary is already self-signed by the release build; re-signing is only
# needed if someone rebuilds it from source.
bundle ohos-arm64 \
  codewhale-ohos-arm64 codew-ohos-arm64 tar.gz ""
bundle macos-x64 \
  codewhale-macos-x64 codew-macos-x64 tar.gz ""
bundle macos-arm64 \
  codewhale-macos-arm64 codew-macos-arm64 tar.gz ""
bundle windows-x64 \
  codewhale-windows-x64.exe codew-windows-x64.exe zip ""
bundle windows-x64 \
  codewhale-windows-x64.exe codew-windows-x64.exe zip portable
bundle windows-arm64 \
  codewhale-windows-arm64.exe codew-windows-arm64.exe zip ""
bundle windows-arm64 \
  codewhale-windows-arm64.exe codew-windows-arm64.exe zip portable

sort -o "${manifest}" "${manifest}"
echo "Bundle checksum manifest:"
cat "${manifest}"
