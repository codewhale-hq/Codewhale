#!/usr/bin/env bash
set -euo pipefail
# CodeWhale Unix installer
# Copies codewhale and codew to ~/.local/bin (or $PREFIX/bin)

modify_path=1
case "${CODEWHALE_NO_MODIFY_PATH:-}" in
    1|true|yes) modify_path=0 ;;
esac
for arg in "$@"; do
    case "$arg" in
        -h|--help)
            echo "Usage: ./install.sh [--no-modify-path]"
            echo "  Installs codewhale and codew to \$PREFIX/bin (default ~/.local/bin)."
            echo "  --no-modify-path  Do not add that directory to your shell profile."
            exit 0
            ;;
        --no-modify-path) modify_path=0 ;;
        *) echo "codewhale install: unknown option: $arg (see --help)" >&2; exit 2 ;;
    esac
done

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
supports_uninstall=0
installed_help="$("$BIN_DIR/codewhale" --help </dev/null 2>/dev/null || true)"
if printf '%s\n' "$installed_help" | grep -E '^[[:space:]]+uninstall([[:space:]]|$)' >/dev/null; then
    supports_uninstall=1
fi
if [[ "$supports_uninstall" == 1 ]]; then
    echo "Uninstall this receipted copy: \"$BIN_DIR/codewhale\" uninstall --yes"
else
    echo "Uninstall: this release has no uninstall command. Remove the files: rm \"$BIN_DIR/codewhale\" \"$BIN_DIR/codew\" \"$BIN_DIR/.codewhale-install.sha256\""
    echo "Your data in ~/.codewhale is separate; delete it only if you want it gone."
fi
if [[ -n "$recovery_dir" ]]; then
    echo "Previous files are kept in $recovery_dir for 30 days. Undo: \"$BIN_DIR/codewhale\" doctor --restore $(basename "$recovery_dir")"
fi
CODEWHALE_TELEMETRY=0 CODEWHALE_NO_UPDATE_CHECK=1 "$BIN_DIR/codewhale" doctor --installs --brief 2>/dev/null || true
echo "Usage reporting is on by default (counts only, never your code or conversations). Turn it off: \"$BIN_DIR/codewhale\" config set telemetry false"
echo "Future updates: \"$BIN_DIR/codewhale\" update"
if [[ "$supports_uninstall" == 1 ]]; then
    echo "Updates install automatically; turn off with: codewhale config set update.mode notify"
fi
if [[ "$(uname -s)" == Darwin ]] && ! command -v node >/dev/null 2>&1; then
    echo ""
    echo "Computer Use is included and needs Node.js 20 or newer on PATH."
    echo "Install Node.js from https://nodejs.org/, then restart Codewhale to enable Computer Use."
fi
path_selected=1
for bin in codewhale codew; do
    resolved="$(command -v "$bin" || true)"
    if [[ "$resolved" != "$BIN_DIR/$bin" ]]; then
        echo "PATH selects ${resolved:-no $bin command}; this install is $BIN_DIR/$bin"
        path_selected=0
    fi
done
say() { printf '%s\n' "$*"; }
install_dir="$BIN_DIR"
case "$(uname -s)" in Darwin) target=macos-host ;; *) target=other ;; esac
path_marker_start="# >>> codewhale PATH >>>"
path_marker_end="# <<< codewhale PATH <<<"

install_dir_on_path() {
  case ":${PATH:-}:" in
    *":$install_dir:"*) return 0 ;;
  esac
  return 1
}

pick_profile() {
  profile_file=""
  profile_shown=""
  [ -n "${HOME:-}" ] || return 0
  case "$1" in
    zsh)
      if [ -n "${ZDOTDIR:-}" ]; then
        case "$ZDOTDIR" in
          /*) ;;
          *) return 0 ;;
        esac
        case "$ZDOTDIR" in
          *[!A-Za-z0-9_./+-]*) return 0 ;;
        esac
        profile_file="$ZDOTDIR/.zshrc"
        profile_shown="\"$profile_file\""
        return 0
      fi
      profile=".zshrc"
      ;;
    bash)
      case "$target" in
        macos-*)
          profile=".bash_profile"
          for candidate in .bash_profile .bash_login .profile; do
            if [ -e "$HOME/$candidate" ]; then
              profile="$candidate"
              break
            fi
          done
          ;;
        *) profile=".bashrc" ;;
      esac
      ;;
    *) profile=".profile" ;;
  esac
  profile_file="$HOME/$profile"
  profile_shown="~/$profile"
}

add_path_block() {
  path_note=""
  path_state=added
  if [ -L "$profile_file" ]; then
    path_note="$profile_shown is a symlink"
    return 1
  fi
  lead_newline=0
  if [ -e "$profile_file" ]; then
    if [ ! -f "$profile_file" ] || [ ! -w "$profile_file" ]; then
      path_note="$profile_shown is not a writable regular file"
      return 1
    fi
    if grep -qxF "$path_marker_start" "$profile_file" 2>/dev/null; then
      path_state=present
      return 0
    fi
    if [ -s "$profile_file" ] && [ -n "$(tail -c 1 "$profile_file" 2>/dev/null)" ]; then
      lead_newline=1
    fi
  else
    profile_parent="$(dirname "$profile_file")"
    if [ ! -d "$profile_parent" ] || [ ! -w "$profile_parent" ]; then
      path_note="the folder for $profile_shown is missing or not writable"
      return 1
    fi
  fi
  if ! {
    if [ "$lead_newline" -eq 1 ]; then printf '\n'; fi
    printf '%s\n' "$path_marker_start" "export PATH=\"$path_dir:\$PATH\"" "$path_marker_end"
  } >> "$profile_file" 2>/dev/null; then
    path_note="could not write to $profile_shown"
    return 1
  fi
  return 0
}

undo_path_block_command() {
  printf '%s' "cw_tmp=\"\$(mktemp)\" && sed -e '/^# >>> codewhale PATH >>>\$/{\$!N;\$!N;/\\n# <<< codewhale PATH <<<\$/d;}' $profile_shown > \"\$cw_tmp\" && cat \"\$cw_tmp\" > $profile_shown; rm -f \"\$cw_tmp\""
}

if [ "$path_selected" -eq 1 ]; then
  say ""
  say "Next: codewhale --version"
else
  path_dir="$install_dir"
  if [ -n "${HOME:-}" ] && [ "$install_dir" = "$(cd -P "$HOME/.local/bin" 2>/dev/null && pwd)" ]; then
    path_dir="\$HOME/.local/bin"
  fi
  shell_name="${SHELL:-}"
  shell_name="${shell_name##*/}"
  case "$path_dir" in
    *[\'\"\`\\\$]*|*"
"*)
      if [ "$path_dir" != "\$HOME/.local/bin" ]; then
        shell_name="unsafe-path"
      fi
      ;;
  esac
  say ""
  if [ -z "$(command -v codewhale 2>/dev/null || true)" ]; then
    say "codewhale is not on your PATH in this shell yet."
  else
    say "A different codewhale comes first on your PATH in this shell."
  fi
  case "$shell_name" in
    fish)
      say "This installer does not edit fish's configuration. Put $install_dir first on PATH in future shells (run once):"
      say "  fish_add_path \"$path_dir\""
      say "It takes effect in this fish shell and in new ones (fish 3.2 or newer)."
      say "PATH help: https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md#put-it-on-your-path"
      say ""
      say "Then check it:"
      say "  codewhale --version"
      ;;
    zsh|bash|sh|dash|ksh|mksh|ash)
      pick_profile "$shell_name"
      persist_line="printf '%s\\n' '$path_marker_start' 'export PATH=\"$path_dir:\$PATH\"' '$path_marker_end' >> $profile_shown"
      if [ -z "$profile_file" ]; then
        say "This installer could not pick a profile file for $shell_name, so it changed nothing. To keep it in future shells (run once):"
        say "  Add $install_dir first to PATH in your shell's startup file."
      elif [ "$modify_path" -eq 0 ]; then
        say "Not editing your shell profile (--no-modify-path). To keep it in future shells, run once:"
        say "  $persist_line"
      elif install_dir_on_path; then
        say "$install_dir is on PATH but not first, so your shell profile was not edited. To put it first in future shells, run once:"
        say "  $persist_line"
      elif add_path_block; then
        if [ "$path_state" = present ]; then
          say "$profile_shown already has the codewhale PATH block; it was left as is."
        else
          say "Added to $profile_shown so new shells find codewhale (skip this with --no-modify-path):"
          say "  $path_marker_start"
          say "  export PATH=\"$path_dir:\$PATH\""
          say "  $path_marker_end"
          say "Only those three lines were added; nothing else in the file was changed. Shells that are already open do not reread the profile."
        fi
        say "To undo, run:"
        say "  $(undo_path_block_command)"
        if [ "$supports_uninstall" -eq 1 ]; then
          say "The marker lines let codewhale uninstall remove exactly this block; a bare export line is yours and is never touched."
        fi
      else
        say "Did not edit your shell profile: $path_note. To keep it in future shells, run once:"
        say "  $persist_line"
        say "To undo that later, delete the three lines between and including the marker lines."
      fi
      say "PATH help: https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md#put-it-on-your-path"
      say ""
      say "Run this now, in this shell:"
      say "  export PATH=\"$path_dir:\$PATH\"; hash -r"
      say "Then check it:"
      say "  codewhale --version"
      ;;
    unsafe-path)
      say "Add $install_dir first to PATH in your shell's startup file; its name contains shell-special characters, so no command line is printed for it and nothing was edited."
      say "PATH help: https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md#put-it-on-your-path"
      ;;
    *)
      say "Add $install_dir first to PATH in your shell's startup file; this installer has no PATH line for ${shell_name:-an unknown shell} and edited nothing."
      say "PATH help: https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md#put-it-on-your-path"
      say ""
      say "Until then, run it by its full path:"
      say "  \"$install_dir/codewhale\" --version"
      ;;
  esac
fi
