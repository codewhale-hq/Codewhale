#!/usr/bin/env bash
set -euo pipefail
# CodeWhale Unix installer
# Copies codewhale and codew to ~/.local/bin (or $PREFIX/bin)

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="${PREFIX}/bin"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        echo "ERROR: sha256sum or shasum is required for the ownership receipt" >&2
        return 1
    fi
}

version_code() {
    local version="$1"
    local major minor patch
    IFS=. read -r major minor patch <<< "$version"
    printf '%d%03d%03d\n' "${major:-0}" "${minor:-0}" "${patch:-0}"
}

detect_host_glibc() {
    local out
    if out="$(getconf GNU_LIBC_VERSION 2>/dev/null)"; then
        printf '%s\n' "$out" | awk '{print $NF; exit}'
        return 0
    fi
    if out="$(ldd --version 2>&1 | head -n 1)"; then
        printf '%s\n' "$out" | grep -Eo '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -n 1
        return 0
    fi
    return 1
}

required_glibc_for_binary() {
    local bin="$1"
    local versions
    versions="$(grep -aoE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' "$bin" 2>/dev/null | sed 's/^GLIBC_//' || true)"
    if [[ -z "$versions" ]]; then
        return 1
    fi
    printf '%s\n' "$versions" | awk -F. '
        {
            patch = ($3 == "" ? 0 : $3)
            code = ($1 * 1000000) + ($2 * 1000) + patch
            if (code > best) {
                best = code
                value = $0
            }
        }
        END {
            if (value != "") print value
        }
    '
}

preflight_glibc() {
    local bin="$1"
    local role="${2:-CLI}"
    if [[ "$(uname -s)" != "Linux" ]]; then
        return 0
    fi
    if [[ "${role}" != "compiled host" && ( "${CODEWHALE_SKIP_GLIBC_CHECK:-}" == "1" || "${DEEPSEEK_TUI_SKIP_GLIBC_CHECK:-}" == "1" || "${DEEPSEEK_SKIP_GLIBC_CHECK:-}" == "1" ) ]]; then
        return 0
    fi

    local required
    if ! required="$(required_glibc_for_binary "$bin")" || [[ -z "$required" ]]; then
        return 0
    fi

    local host
    if ! host="$(detect_host_glibc)" || [[ -z "$host" ]]; then
        echo "ERROR: $(basename "$bin") requires GLIBC_$required, but no GNU libc was detected." >&2
        if [[ "$(basename "$bin")" == codewhale-extension-host ]]; then
            echo "The optional compiled Bun image has a separate libc floor; use Node on this installation. The Codewhale CLI remains static musl." >&2
        else
            echo "Official Codewhale Linux release assets (x64 and arm64) are static musl builds" >&2
            echo "with no glibc dependency, so this binary is not an official release asset." >&2
        fi
        echo "Check where it came from, or build from source on this host." >&2
        echo "Build from source instead: cargo install codewhale-cli --locked" >&2
        if [[ "$role" != "compiled host" ]]; then echo "Set CODEWHALE_SKIP_GLIBC_CHECK=1 to bypass this check at your own risk." >&2; fi
        return 1
    fi

    if [[ "$(version_code "$host")" -lt "$(version_code "$required")" ]]; then
        echo "ERROR: $(basename "$bin") requires GLIBC_$required, but this system has glibc $host." >&2
        if [[ "$(basename "$bin")" == codewhale-extension-host ]]; then
            echo "The optional compiled Bun image has a separate libc floor; use Node on this installation. The Codewhale CLI remains static musl." >&2
        else
            echo "Official Codewhale Linux release assets (x64 and arm64) are static musl builds" >&2
            echo "with no glibc dependency, so this binary is not an official release asset." >&2
        fi
        echo "Check where it came from, or build from source on this host." >&2
        echo "Build from source instead: cargo install codewhale-cli --locked" >&2
        if [[ "$role" != "compiled host" ]]; then echo "Set CODEWHALE_SKIP_GLIBC_CHECK=1 to bypass this check at your own risk." >&2; fi
        return 1
    fi
}

# This script installs an already checksum-verified archive. An earlier
# Codewhale in BIN_DIR is upgraded: its files move to the recovery folder
# (~/.codewhale/recovery, kept 30 days), never beside the commands on PATH.
case "$BIN_DIR" in
    /*) ;;
    *) echo "ERROR: PREFIX must be an absolute user path" >&2; exit 1 ;;
esac
case "/$BIN_DIR/" in
    */./*|*/../*) echo "ERROR: PREFIX must not contain . or .. path components" >&2; exit 1 ;;
esac
[[ ! -L "$BIN_DIR" ]] || { echo "ERROR: $BIN_DIR is a symlink; choose a fresh PREFIX" >&2; exit 1; }
preflight_dir="$BIN_DIR"
install_suffix=""
while [[ ! -d "$preflight_dir" ]]; do
    [[ ! -e "$preflight_dir" && ! -L "$preflight_dir" ]] || { echo "ERROR: not a directory: $preflight_dir" >&2; exit 1; }
    install_suffix="/$(basename "$preflight_dir")$install_suffix"
    preflight_dir="$(dirname "$preflight_dir")"
done
preflight_dir="$(cd -P "$preflight_dir" && pwd)"
BIN_DIR="$preflight_dir$install_suffix"
case "$BIN_DIR/" in
    /bin/*|/sbin/*|/usr/bin/*|/usr/sbin/*|/nix/store/*|/gnu/store/*|*/node_modules/*|*/Cellar/*|*/.linuxbrew/*|*/linuxbrew/*|*/.cargo/bin/*|/opt/homebrew/bin/*|/usr/local/bin/*|/opt/local/bin/*)
        echo "ERROR: refusing managed/system directory $BIN_DIR; choose a fresh user PREFIX" >&2
        exit 1
        ;;
esac
[[ -O "$preflight_dir" && -w "$preflight_dir" ]] || { echo "ERROR: $preflight_dir must be a writable directory owned by you (no sudo)" >&2; exit 1; }
if [[ -n "$(find "$preflight_dir" -prune \( -perm -020 -o -perm -002 \) -print)" ]]; then
    echo "ERROR: $preflight_dir is writable by its group or others; choose a private PREFIX" >&2
    exit 1
fi
for cargo_receipt in "$(dirname "$BIN_DIR")/.crates.toml" "$(dirname "$BIN_DIR")/.crates2.json"; do
    [[ ! -e "$cargo_receipt" && ! -L "$cargo_receipt" ]] || { echo "ERROR: Cargo install metadata beside this directory; choose a fresh PREFIX" >&2; exit 1; }
done

(umask 077; mkdir -p "$BIN_DIR")
[[ ! -L "$BIN_DIR" ]] || { echo "ERROR: $BIN_DIR changed to a symlink" >&2; exit 1; }
BIN_DIR="$(cd -P "$BIN_DIR" && pwd)"

check_destination() {
    local src="$1" dst="$2" mode="${3:-0755}"
    destination_exists=0
    if [[ -e "$dst" || -L "$dst" ]]; then
        if [[ ! -L "$dst" && -f "$dst" ]] && [[ "$mode" != 0755 || -x "$dst" ]] && cmp -s "$src" "$dst"; then
            destination_exists=1
            return 0
        fi
        local upgradable=0
        if [[ ! -L "$dst" && -f "$dst" && -O "$dst" ]]; then
            case "$(basename "$dst")" in
                codewhale|codew) grep -aq -e 'codewhale-updater' -e 'deepseek-tui' "$dst" 2>/dev/null && upgradable=1 ;;
                .codewhale-install.sha256) upgradable=1 ;;
            esac
            [[ "${CODEWHALE_FORCE_REINSTALL:-}" != 1 ]] || upgradable=1
        fi
        if [[ "$upgradable" == 1 ]]; then
            move_to_recovery "$dst"
            echo "Upgrading $dst (previous file kept in $recovery_dir for 30 days)"
            return 0
        fi
        echo "ERROR: $dst is not a Codewhale release file this installer can upgrade; it was not changed." >&2
        echo "It is a symlink, belongs to another user, or is a different program." >&2
        echo "Install into a fresh prefix instead:" >&2
        echo '  PREFIX="$HOME/.local/codewhale" ./install.sh' >&2
        echo "Or move a regular file you own to the recovery folder and install anyway:" >&2
        echo '  CODEWHALE_FORCE_REINSTALL=1 ./install.sh' >&2
        return 1
    fi
}

if [[ -n "${CODEWHALE_HOME:-}" && "$CODEWHALE_HOME" == /* ]]; then
    recovery_root="$CODEWHALE_HOME/recovery"
else
    recovery_root="$HOME/.codewhale/recovery"
fi
recovery_dir=""
recovery_seq=0
open_recovery() {
    [[ -z "$recovery_dir" ]] || return 0
    mkdir -p "$recovery_root"
    chmod 700 "$recovery_root" 2>/dev/null || true
    local stamp candidate attempt=1
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    candidate="$stamp"
    until mkdir "$recovery_root/$candidate" 2>/dev/null; do
        (( attempt < 100 )) || { echo "ERROR: cannot create a recovery folder under $recovery_root" >&2; exit 1; }
        candidate="$stamp-$attempt"
        attempt=$((attempt + 1))
    done
    recovery_dir="$recovery_root/$candidate"
}
move_to_recovery() {
    local file="$1" stored digest size
    open_recovery
    recovery_seq=$((recovery_seq + 1))
    stored="$recovery_dir/$(printf '%03d' "$recovery_seq")-$(basename "$file")"
    digest="$(sha256_file "$file" | tr '[:upper:]' '[:lower:]')"
    size="$(wc -c < "$file" | tr -d ' ')"
    mv "$file" "$stored"
    printf '%s\t%s\t%s\t%s\n' "$file" "$stored" "$digest" "$size" >> "$recovery_dir/manifest.tsv"
    forced_backups+=("$stored"$'\t'"$file")
}

# The image is an explicit installer choice; presence never changes Core's
# Node default. Archives carry notices/source beside any qualified image.
receipt_tmp="$(mktemp -d "$BIN_DIR/.codewhale-receipt.XXXXXX")"
receipt_name=".codewhale-install.sha256"
forced_backups=()
stage=""
stage_dir=""
# Commands this run published, as "source<TAB>destination" lines. If a later
# publication fails, they are removed again (only while each is still the
# exact file this run wrote), so a failed install leaves no half-installed
# pair; files that were already installed are never touched.
published=""
rollback_published() {
    local src dst
    while IFS=$'\t' read -r src dst; do
        [[ -n "$dst" ]] || continue
        if [[ ! -L "$dst" && -f "$dst" ]] && cmp -s "$src" "$dst"; then
            rm -f "$dst"
            echo "Removed $dst: this install did not complete." >&2
        fi
    done <<< "$published"
}
on_exit() {
    local status=$? dst backup
    if [[ -n "$stage" ]]; then rm -f "$stage"; fi
    if [[ -n "$stage_dir" ]]; then rmdir "$stage_dir"; fi
    if [[ "$status" -ne 0 && -n "$published" ]]; then rollback_published; fi
    if [[ "$status" -ne 0 ]]; then
        for entry in ${forced_backups[@]+"${forced_backups[@]}"}; do
            backup="${entry%%$'\t'*}"
            dst="${entry#*$'\t'}"
            [[ -e "$backup" ]] || continue
            if [[ ! -e "$dst" && ! -L "$dst" ]]; then
                mv "$backup" "$dst" && echo "Restored $dst: this install did not complete." >&2
            fi
        done
    fi
    rm -f "$receipt_tmp/$receipt_name"
    rmdir "$receipt_tmp"
}
trap on_exit EXIT
payloads=(codewhale codew)
if [[ "${CODEWHALE_INSTALL_COMPILED_HOST:-}" == 1 ]]; then
    for file in codewhale-extension-host codewhale-extension-host.LICENSES.txt codewhale-extension-host.relink-source.tar.gz codewhale-extension-host.release.json; do
        [[ -f "$SCRIPT_DIR/$file" && ! -L "$SCRIPT_DIR/$file" ]] || { echo "ERROR: compiled host requested but archive has no complete qualified payload ($file); use Node" >&2; exit 1; }
        payloads+=("$file")
    done
fi
# Validate every source and destination before the first write.
for bin in "${payloads[@]}"; do
    src="$SCRIPT_DIR/$bin"
    [[ -f "$src" ]] || { echo "ERROR: $src not found in archive" >&2; exit 1; }
    mode=0644
    case "$bin" in codewhale|codew|codewhale-extension-host) mode=0755; preflight_glibc "$src" "${bin/codewhale-extension-host/compiled host}" ;; esac
    check_destination "$src" "$BIN_DIR/$bin" "$mode"
done

for command_name in codewhale codew; do
    command_digest="$(sha256_file "$SCRIPT_DIR/$command_name" | tr '[:upper:]' '[:lower:]')"
    [[ "$command_digest" =~ ^[0-9a-f]{64}$ ]] || { echo "ERROR: invalid command digest for $command_name" >&2; exit 1; }
    printf '%s  %s\n' "$command_digest" "$command_name"
done > "$receipt_tmp/$receipt_name"
check_destination "$receipt_tmp/$receipt_name" "$BIN_DIR/$receipt_name" 0644

install_binary() {
    local src="$1" dst="$2" mode="${3:-0755}"
    check_destination "$src" "$dst" "$mode"
    if [[ "$destination_exists" == 1 ]]; then
        echo "  $dst (already installed)"
        return
    fi
    stage_dir="$(mktemp -d "$BIN_DIR/.codewhale-install.XXXXXX")"
    stage="$stage_dir/$(basename "$dst")"
    cp "$src" "$stage"
    chmod "$mode" "$stage"
    # Same-directory hard-link publication is atomic and never overwrites a
    # destination created between preflight and this operation.
    # The explicit parent operand avoids treating a raced-in destination
    # directory (or directory symlink) as an alternate publication location.
    ln "$stage" "$BIN_DIR/"
    [[ ! -L "$dst" && -f "$dst" ]] && cmp -s "$stage" "$dst" || {
        echo "ERROR: installed path changed during publication: $dst" >&2
        return 1
    }
    published+="$src"$'\t'"$dst"$'\n'
    rm -f "$stage"
    rmdir "$stage_dir"
    stage=""
    stage_dir=""
    echo "  $dst"
}

echo "Installing codewhale to $BIN_DIR ..."
for bin in "${payloads[@]}"; do
    mode=0644
    case "$bin" in codewhale|codew|codewhale-extension-host) mode=0755 ;; esac
    install_binary "$SCRIPT_DIR/$bin" "$BIN_DIR/$bin" "$mode"
done

echo ""
install_binary "$receipt_tmp/$receipt_name" "$BIN_DIR/$receipt_name" 0644
echo "Done. Commands installed to $BIN_DIR."
echo "Uninstall this receipted copy: \"$BIN_DIR/codewhale\" uninstall --yes"
if [[ -n "$recovery_dir" ]]; then
    echo "Previous files are kept in $recovery_dir for 30 days. Undo: \"$BIN_DIR/codewhale\" doctor --restore $(basename "$recovery_dir")"
fi
"$BIN_DIR/codewhale" doctor --installs --brief 2>/dev/null || true
echo "Future updates: \"$BIN_DIR/codewhale\" update"
echo "Updates install automatically; turn off with: codewhale config set update.mode notify"
for bin in codewhale codew; do
    resolved="$(command -v "$bin" || true)"
    if [[ "$resolved" != "$BIN_DIR/$bin" ]]; then
        echo "PATH selects ${resolved:-no $bin command}; this install is $BIN_DIR/$bin"
    fi
done
echo "To select this installation in the current shell:"
echo "  export PATH=\"$BIN_DIR:\$PATH\""
echo "  hash -r"
echo "  command -v codewhale codew"
echo "Keep the directory first in your shell profile after verifying it."
if ! command -v node >/dev/null 2>&1; then
    echo "Computer Use is included and needs Node.js 20 or newer on PATH."
    echo "Install Node.js from https://nodejs.org/, then restart Codewhale to enable Computer Use."
fi
