#!/bin/sh
set -eu

repo="codewhale-hq/CodeWhale"
version="${CODEWHALE_VERSION:-latest}"
release_base="${CODEWHALE_RELEASE_BASE_URL:-${DEEPSEEK_TUI_RELEASE_BASE_URL:-}}"

usage() {
  cat <<'USAGE'
Codewhale GitHub release installer for new macOS and Linux installations.
For an existing direct install, run its codewhale update command.

Usage:
  curl -fsSL https://codewhale.net/install.sh | sh

Environment:
  CODEWHALE_INSTALL_DIR    Install directory. Default: $HOME/.local/bin
  CODEWHALE_VERSION        Release tag for a fresh directory. Default: latest
  CODEWHALE_FORCE_REINSTALL=1
                           Replace differing codewhale/codew already in the install
                           directory, keeping each old file beside it as .bak-<time>
  CODEWHALE_RELEASE_BASE_URL
                           Custom release asset base URL ending in /download
  CODEWHALE_INSTALL_COMPILED_HOST=1
                           Also install a qualified optional Bun image + notices/source
                           Node remains the default; missing eligibility fails loudly
  CODEWHALE_SKIP_GLIBC_CHECK=1
                           Skip Linux arm64 glibc compatibility preflight

Examples:
  curl -fsSL https://codewhale.net/install.sh | CODEWHALE_INSTALL_DIR="$HOME/.local/codewhale/bin" sh
  curl -fsSL https://codewhale.net/install.sh | CODEWHALE_VERSION=vX.Y.Z sh
USAGE
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
esac

say() {
  printf '%s\n' "$*"
}

fail() {
  printf 'codewhale install: %s\n' "$*" >&2
  exit 1
}

if [ -n "${CODEWHALE_INSTALL_DIR:-}" ]; then
  install_dir="$CODEWHALE_INSTALL_DIR"
else
  [ -n "${HOME:-}" ] || fail "HOME is not set; set CODEWHALE_INSTALL_DIR"
  install_dir="$HOME/.local/bin"
fi

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

download() {
  url="$1"
  out="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$out"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$out"
  else
    fail "curl or wget is required"
  fi
}

sha256_file() {
  file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    fail "sha256sum or shasum is required to verify downloads"
  fi
}

verify_asset() {
  asset="$1"
  file="$2"
  manifest="$3"
  expected="$(
    awk -v name="$asset" '
      {
        digest = tolower($1)
        file = $2
        sub(/^\*/, "", file)
        if (file == name && digest ~ /^[0-9a-f]{64}$/) {
          print digest
          exit
        }
      }
    ' "$manifest"
  )"
  [ -n "$expected" ] || fail "checksum not found for $asset"
  actual="$(sha256_file "$file" | tr '[:upper:]' '[:lower:]')"
  [ "$actual" = "$expected" ] || fail "checksum mismatch for $asset"
}

glibc_version() {
  if command -v getconf >/dev/null 2>&1; then
    getconf GNU_LIBC_VERSION 2>/dev/null | awk '{ print $NF; exit }'
    return
  fi
  if command -v ldd >/dev/null 2>&1; then
    ldd --version 2>/dev/null | awk 'NR == 1 {
      for (i = 1; i <= NF; i++) {
        if ($i ~ /^[0-9]+\.[0-9]+/) {
          print $i
          exit
        }
      }
    }'
  fi
}

version_at_least() {
  have="$1"
  need="$2"
  awk -v have="$have" -v need="$need" '
    BEGIN {
      split(have, h, ".")
      split(need, n, ".")
      for (i = 1; i <= 3; i++) {
        hv = h[i] + 0
        nv = n[i] + 0
        if (hv > nv) exit 0
        if (hv < nv) exit 1
      }
      exit 0
    }
  '
}

check_glibc() {
  case "$target" in
    linux-arm64) ;;
    *) return ;;
  esac

  # Linux arm64 assets became static musl builds in v0.9.6. `latest` and
  # explicit v0.9.6+ installs therefore have no glibc floor. Keep the
  # preflight only for explicitly requested older releases, whose arm64
  # assets were linked against GNU libc on Ubuntu 24.04.
  if [ "$version" = "latest" ]; then
    return
  fi
  numeric_version="${version#v}"
  if awk -v have="$numeric_version" '
    BEGIN {
      if (have !~ /^[0-9]+\.[0-9]+\.[0-9]+$/) exit 1
      split(have, h, ".")
      if (h[1] > 0) exit 0
      if (h[1] < 0) exit 1
      if (h[2] > 9) exit 0
      if (h[2] < 9) exit 1
      exit !(h[3] >= 6)
    }
  '; then
    return
  fi

  [ "${CODEWHALE_SKIP_GLIBC_CHECK:-}" = "1" ] && return
  [ "${DEEPSEEK_TUI_SKIP_GLIBC_CHECK:-}" = "1" ] && return
  [ "${DEEPSEEK_SKIP_GLIBC_CHECK:-}" = "1" ] && return

  required="2.39"
  host="$(glibc_version || true)"
  if [ -z "$host" ] || ! version_at_least "$host" "$required"; then
    cat >&2 <<EOF
codewhale install: Codewhale $version $target assets require glibc $required or newer.
This system reports glibc ${host:-unavailable}.

Linux arm64 assets before v0.9.6 were GNU libc builds from Ubuntu 24.04.
Current v0.9.6+ assets are static musl builds. Build this older release from
source with Cargo or set
CODEWHALE_SKIP_GLIBC_CHECK=1 to bypass this check at your own risk.
EOF
    exit 1
  fi
}

detect_platform() {
  os="$(uname -s)"
  arch="$(uname -m)"

  if [ -n "${TERMUX_VERSION:-}" ] || [ "$(uname -o 2>/dev/null || true)" = "Android" ]; then
    fail "Android/Termux needs the Android arm64 preview archive, not a Linux binary. See https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md"
  fi

  case "$os" in
    Darwin) platform="macos" ;;
    Linux) platform="linux" ;;
    *) fail "unsupported OS: $os. Use the matching asset at https://github.com/codewhale-hq/CodeWhale/releases/latest; npm and Cargo are secondary options." ;;
  esac

  case "$arch" in
    x86_64|amd64) cpu="x64" ;;
    arm64|aarch64) cpu="arm64" ;;
    riscv64) fail "Linux riscv64 prebuilt assets are temporarily unavailable because the locked rquickjs-sys dependency does not ship riscv64gc bindings." ;;
    *) fail "unsupported CPU architecture: $arch. Use Cargo or build from source." ;;
  esac

  printf '%s-%s' "$platform" "$cpu"
}

if [ -z "$release_base" ]; then
  if [ "$version" = "latest" ]; then
    release_base="https://github.com/$repo/releases/latest/download"
  else
    release_base="https://github.com/$repo/releases/download/$version"
  fi
fi

target="$(detect_platform)"
check_glibc
cli_asset="codewhale-$target"
shim_asset="codew-$target"
manifest_asset="codewhale-artifacts-sha256.txt"

# Resolve the real directory before applying managed-prefix checks. Never use
# sudo or allow an install directory symlink to obscure which files will change.
case "$install_dir" in
  /*) ;;
  *) fail "CODEWHALE_INSTALL_DIR must be an absolute path" ;;
esac
[ ! -L "$install_dir" ] || fail "install directory is a symlink: $install_dir; choose a fresh user directory"
mkdir -p "$install_dir" || fail "cannot create $install_dir; choose a writable user directory (no sudo is used)"
install_dir="$(cd -P "$install_dir" && pwd)"
case "$install_dir/" in
  /bin/*|/sbin/*|/usr/bin/*|/usr/sbin/*|/nix/store/*|/gnu/store/*|*/node_modules/*|*/Cellar/*|*/.linuxbrew/*|*/linuxbrew/*|*/.cargo/bin/*)
    fail "refusing managed/system directory $install_dir; use a fresh user directory"
    ;;
esac
[ -w "$install_dir" ] || fail "$install_dir is not writable; choose a user directory (no sudo is used)"
for cargo_receipt in "$install_dir/../.crates.toml" "$install_dir/../.crates2.json"; do
  if [ -e "$cargo_receipt" ] || [ -L "$cargo_receipt" ]; then
    fail "Cargo install metadata beside this directory; use a fresh directory instead"
  fi
done

tmpdir="$(mktemp -d 2>/dev/null || mktemp -d -t codewhale-install)"
trap 'rm -rf "$tmpdir"' EXIT INT TERM

forced_backups="$tmpdir/.forced-backups"
: > "$forced_backups"

stage=""
stage_dir=""
# Commands this run published. If a later publication fails they are removed
# again, but only while each is still the exact file this run wrote, so a
# failed install leaves no half-installed pair and never touches a file that
# was already installed.
published="$tmpdir/.published"
: > "$published"
rollback_published() {
  while IFS= read -r name; do
    destination="$install_dir/$name"
    if [ ! -L "$destination" ] && [ -f "$destination" ] && cmp -s "$tmpdir/$name" "$destination"; then
      rm -f "$destination"
      say "Removed $destination: this install did not complete." >&2
    fi
  done < "$published"
}
restore_forced_backups() {
  [ -f "$forced_backups" ] || return 0
  while IFS= read -r backup; do
    [ -n "$backup" ] || continue
    [ -e "$backup" ] || continue
    destination="$(printf '%s' "$backup" | sed 's/\.bak-[0-9TZ]*\(-[0-9][0-9]*\)\{0,1\}$//')"
    if [ ! -e "$destination" ] && [ ! -L "$destination" ]; then
      mv "$backup" "$destination" && say "Restored $destination: this install did not complete." >&2
    fi
  done < "$forced_backups"
}
on_exit() {
  status=$?
  if [ -n "$stage" ]; then rm -f "$stage"; fi
  if [ -n "$stage_dir" ]; then rmdir "$stage_dir"; fi
  if [ "$status" -ne 0 ]; then
    rollback_published
    restore_forced_backups
  fi
  rm -rf "$tmpdir"
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

say "Installing Codewhale for $target"
say "Release assets: $release_base"
say "Install dir: $install_dir"

download "$release_base/$manifest_asset" "$tmpdir/$manifest_asset"
download "$release_base/$cli_asset" "$tmpdir/codewhale"
download "$release_base/$shim_asset" "$tmpdir/codew"

verify_asset "$cli_asset" "$tmpdir/codewhale" "$tmpdir/$manifest_asset"
verify_asset "$shim_asset" "$tmpdir/codew" "$tmpdir/$manifest_asset"
say "Checksums verified"

chmod 755 "$tmpdir/codewhale" "$tmpdir/codew"
if command -v xattr >/dev/null 2>&1; then
  xattr -d com.apple.quarantine "$tmpdir/codewhale" "$tmpdir/codew" 2>/dev/null || true
fi

check_destination() {
  destination="$1"
  source="$2"
  mode="${3:-755}"
  destination_exists=0
  if [ -e "$destination" ] || [ -L "$destination" ]; then
    if [ ! -L "$destination" ] && [ -f "$destination" ] && { [ "$mode" != 755 ] || [ -x "$destination" ]; } && cmp -s "$source" "$destination"; then
      destination_exists=1
      return
    fi
    if [ "${CODEWHALE_FORCE_REINSTALL:-}" = 1 ] && [ ! -L "$destination" ] && [ -f "$destination" ]; then
      backup="$destination.bak-$(date -u +%Y%m%dT%H%M%SZ)"
      suffix=1
      while [ -e "$backup" ] || [ -L "$backup" ]; do
        backup="$destination.bak-$(date -u +%Y%m%dT%H%M%SZ)-$suffix"
        suffix=$((suffix + 1))
      done
      mv "$destination" "$backup" || fail "cannot move $destination aside; it was not replaced"
      printf '%s\n' "$backup" >> "$forced_backups"
      say "Replacing $destination (previous file kept at $backup)"
      return
    fi
    cat >&2 <<EOF
codewhale install: refusing to replace existing $destination.
It may be a newer build, another installation, or a symlink. No existing file was changed.
For an existing direct Codewhale install, run its full path with 'update'.
To replace it with this release and keep the previous file beside it:
  curl -fsSL https://codewhale.net/install.sh | CODEWHALE_FORCE_REINSTALL=1 sh
To migrate safely from a package manager or mixed installation, create a fresh directory:
  mkdir -p "\$HOME/.local"
  codewhale_install_dir="\$(mktemp -d "\$HOME/.local/codewhale-release.XXXXXX")"
  curl -fsSL https://codewhale.net/install.sh | CODEWHALE_INSTALL_DIR="\$codewhale_install_dir" sh
  "\$codewhale_install_dir/codewhale" --version
  export PATH="\$codewhale_install_dir:\$PATH"
  hash -r
  command -v codewhale codew
EOF
    exit 1
  fi
}

# Check every command before publishing any of them. Existing identical release
# files are an idempotent install; anything different uses the canonical updater.
# An enabled companion must exist in this same checksummed release. No
# installer fetches a Bun runtime or invents Android/musl/cross-arch support.
if [ "${CODEWHALE_INSTALL_COMPILED_HOST:-}" = 1 ]; then
  host_asset="codewhale-extension-host-$target"
  for companion in "$host_asset" "$host_asset-LICENSES.txt" "$host_asset-relink-source.tar.gz" codewhale-extension-hosts.json; do
    download "$release_base/$companion" "$tmpdir/$companion"
    verify_asset "$companion" "$tmpdir/$companion" "$tmpdir/codewhale-artifacts-sha256.txt"
  done
  mv "$tmpdir/$host_asset" "$tmpdir/codewhale-extension-host"
  mv "$tmpdir/$host_asset-LICENSES.txt" "$tmpdir/codewhale-extension-host.LICENSES.txt"
  mv "$tmpdir/$host_asset-relink-source.tar.gz" "$tmpdir/codewhale-extension-host.relink-source.tar.gz"
  mv "$tmpdir/codewhale-extension-hosts.json" "$tmpdir/codewhale-extension-host.release.json"
  if [ "$target" = linux-x64 ] || [ "$target" = linux-arm64 ]; then
    required="$(grep -aoE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' "$tmpdir/codewhale-extension-host" 2>/dev/null | sed 's/GLIBC_//' | awk -F. '{ code=$1*1000000+$2*1000+$3; if(code>best){best=code; value=$0} } END {print value}' || true)"
    if [ -n "$required" ]; then
      available="$(glibc_version || true)"
      [ -n "$available" ] && version_at_least "$available" "$required" || fail "optional Bun host requires GLIBC_$required; CLI remains static musl. Use Node on this installation."
    fi
  fi
  for companion in codewhale-extension-host codewhale-extension-host.LICENSES.txt codewhale-extension-host.relink-source.tar.gz codewhale-extension-host.release.json; do
    mode=644
    [ "$companion" != codewhale-extension-host ] || mode=755
    check_destination "$install_dir/$companion" "$tmpdir/$companion" "$mode"
  done
fi

receipt_name=".codewhale-install.sha256"
for command_name in codewhale codew; do
  command_digest="$(sha256_file "$tmpdir/$command_name" | tr '[:upper:]' '[:lower:]')"
  [ "${#command_digest}" -eq 64 ] || fail "invalid command digest for $command_name"
  case "$command_digest" in *[!0-9a-f]*) fail "invalid command digest for $command_name" ;; esac
  printf '%s  %s\n' "$command_digest" "$command_name"
done > "$tmpdir/$receipt_name"
if [ -e "$install_dir/$receipt_name" ] || [ -L "$install_dir/$receipt_name" ]; then
  [ ! -L "$install_dir/$receipt_name" ] && [ -f "$install_dir/$receipt_name" ] && cmp -s "$tmpdir/$receipt_name" "$install_dir/$receipt_name" || fail "existing command receipt differs; use this installation's updater or a fresh directory"
fi
check_destination "$install_dir/$receipt_name" "$tmpdir/$receipt_name" 644
check_destination "$install_dir/codewhale" "$tmpdir/codewhale"
check_destination "$install_dir/codew" "$tmpdir/codew"

install_binary() {
  source="$1"
  destination="$2"
  # Recheck immediately before publication. Never replace a file another
  # process created since preflight: linking the staged inode is no-clobber.
  check_destination "$destination" "$source" "${3:-755}"
  if [ "$destination_exists" -eq 1 ]; then
    say "Already installed: $destination"
    return
  fi
  stage_dir="$(mktemp -d "$install_dir/.codewhale-install.XXXXXX")"
  stage="$stage_dir/$(basename "$destination")"
  cp "$source" "$stage"
  chmod "${3:-755}" "$stage"
  # Pass the intended parent as the directory operand. Passing destination
  # itself would make ln treat a raced-in directory/symlink as a container.
  ln "$stage" "$install_dir/" || fail "destination appeared during install: $destination; it was not replaced"
  [ ! -L "$destination" ] && [ -f "$destination" ] && cmp -s "$stage" "$destination" || fail "installed path changed during publication: $destination"
  basename "$destination" >> "$published"
  rm -f "$stage"
  rmdir "$stage_dir"
  stage=""
  stage_dir=""
}

install_binary "$tmpdir/codewhale" "$install_dir/codewhale"
install_binary "$tmpdir/codew" "$install_dir/codew"
if [ "${CODEWHALE_INSTALL_COMPILED_HOST:-}" = 1 ]; then
  install_binary "$tmpdir/codewhale-extension-host" "$install_dir/codewhale-extension-host"
  for companion in codewhale-extension-host.LICENSES.txt codewhale-extension-host.relink-source.tar.gz codewhale-extension-host.release.json; do
    install_binary "$tmpdir/$companion" "$install_dir/$companion" 644
  done
  say "Installed qualified opt-in image and its notice/relink-source closure; Node remains default."
fi

install_binary "$tmpdir/$receipt_name" "$install_dir/$receipt_name" 644
say "Uninstall this receipted copy: \"$install_dir/codewhale\" uninstall --yes"
say "Installed checksummed release commands:"
say "  $install_dir/codewhale"
say "  $install_dir/codew"

say ""
say "Use this installation: \"$install_dir/codewhale\""
say "Future updates: \"$install_dir/codewhale\" update"
path_selected=1
for command_name in codewhale codew; do
  resolved="$(command -v "$command_name" 2>/dev/null || true)"
  if [ "$resolved" != "$install_dir/$command_name" ]; then
    say "PATH selects ${resolved:-no $command_name command}; this install is $install_dir/$command_name"
    path_selected=0
  fi
done
if [ "$path_selected" -eq 0 ]; then
  # Print the persistent line for the user's login shell. The installer never
  # edits shell profiles itself; the user runs the line once.
  path_dir="$install_dir"
  if [ -n "${HOME:-}" ] && [ "$install_dir" = "$(cd -P "$HOME/.local/bin" 2>/dev/null && pwd)" ]; then
    path_dir="\$HOME/.local/bin"
  fi
  shell_name="${SHELL:-}"
  shell_name="${shell_name##*/}"
  say ""
  case "$path_dir" in
    *[\'\"\`\\\$]*|*"
"*)
      # Only the literal $HOME form may carry a shell-special character. Any
      # other one would break the printed quoting, or run as a command on
      # every shell start once the line is in a profile.
      if [ "$path_dir" != "\$HOME/.local/bin" ]; then
        shell_name="unsafe-path"
      fi
      ;;
  esac
  case "$shell_name" in
    fish)
      say "Put $install_dir first on PATH in future shells (run once; this installer does not edit shell profiles):"
      say "  fish_add_path \"$path_dir\""
      say "It takes effect in this fish shell and in new ones (fish 3.2 or newer)."
      ;;
    zsh|bash|sh|dash|ksh|mksh|ash|"")
      case "$shell_name" in
        zsh) profile=".zshrc" ;;
        bash)
          case "$target" in
            # Login bash reads the first of these that exists; creating
            # ~/.bash_profile would stop an existing ~/.profile from loading.
            macos-*)
              profile=".bash_profile"
              for candidate in .bash_profile .bash_login .profile; do
                if [ -n "${HOME:-}" ] && [ -e "$HOME/$candidate" ]; then
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
      say "Put $install_dir first on PATH in future shells (run once; this installer does not edit shell profiles):"
      say "  echo 'export PATH=\"$path_dir:\$PATH\"' >> ~/$profile"
      say "Then run: . ~/$profile   (or open a new terminal)"
      say "Or for this shell only:"
      say "  export PATH=\"$path_dir:\$PATH\"; hash -r"
      ;;
    unsafe-path)
      say "Add $install_dir first to PATH in your shell's startup file; its name contains shell-special characters, so no command line is printed for it."
      ;;
    *)
      say "Add $install_dir first to PATH in your shell's startup file; this installer has no PATH line for $shell_name."
      ;;
  esac
  say "Verify: command -v codewhale codew"
  say "PATH help: https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md#put-it-on-your-path"
fi
if ! command -v node >/dev/null 2>&1; then
  say "Computer Use is included and needs Node.js 20 or newer on PATH."
  say "Install Node.js from https://nodejs.org/, then restart Codewhale to enable Computer Use."
fi
