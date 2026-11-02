#!/usr/bin/env bash
# Generate the GitHub Release body for a tag.
#
# Usage: generate-release-body.sh <vX.Y.Z> [path/to/CHANGELOG.md]
#
# The install/verify sections are static; the release notes and contributor
# credits come from the CHANGELOG section for the version, so they can never
# drift the way a hand-edited workflow body does.
set -euo pipefail

tag="${1:?usage: $0 <vX.Y.Z> [CHANGELOG.md]}"
changelog="${2:-CHANGELOG.md}"
version="${tag#v}"

section="$(awk -v version="${version}" '
  index($0, "## [" version "]") == 1 { in_section = 1; next }
  in_section && /^## \[/ { exit }
  in_section { print }
' "${changelog}")"

contributors="$(printf '%s\n' "${section}" | awk '
  /^### Contributors[[:space:]]*$/ { in_contributors = 1; next }
  in_contributors && /^### / { exit }
  in_contributors { print }
')"

notes="$(printf '%s\n' "${section}" | awk '
  /^### Contributors[[:space:]]*$/ { in_contributors = 1; next }
  in_contributors && /^### / { in_contributors = 0 }
  !in_contributors { print }
')"

summary="$(printf '%s\n' "${notes}" | awk '/^### / { in_details = 1 } !in_details { print }')"
details="$(printf '%s\n' "${notes}" | awk '/^### / { in_details = 1 } in_details { print }')"
if [[ -z "${summary//[[:space:]]/}" ]]; then
  summary="${notes}"
  details=""
fi

render_body() {
printf "## What's in %s\n\n" "${tag}"
if [[ -n "${summary}" ]]; then
  printf '%s\n' "${summary}"
else
  printf '%s\n' "See the changelog link below for this release's notes."
fi

cat <<EOF

## Install

### Recommended — official GitHub release

New macOS/Linux install (checksummed binaries from this release):

\`\`\`bash
curl -fsSL https://codewhale.net/install.sh | CODEWHALE_VERSION="${tag}" sh
"\$HOME/.local/bin/codewhale" --version
\`\`\`

For Windows, use the matching installer or archive below. For an existing
direct install, run \`codewhale update\`; it prints the executable path and
keeps newer builds. If the install directory is occupied by a different build,
use the fresh-directory migration in [the installation guide](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md).

### Secondary packaging — npm and Cargo

\`\`\`bash
npm install -g codewhale
# or build from source
cargo install codewhale-cli --locked
\`\`\`

The wrapper downloads the matched \`codewhale\` and \`codew\` command assets
from this Release. Both contain the same compiled runtime.

### Docker / GHCR

\`\`\`bash
docker run --rm -it \\
  -e DEEPSEEK_API_KEY="\$DEEPSEEK_API_KEY" \\
  -v codewhale-home:/home/codewhale/.codewhale \\
  ghcr.io/codewhale-hq/codewhale:${tag}
\`\`\`

The image exposes the same runtime as both \`codewhale\` and \`codew\`. The
\`latest\` tag is also updated on release.

### Cargo (Linux / macOS)

\`\`\`bash
cargo install codewhale-cli --locked
\`\`\`

The Cargo package installs \`codewhale\`. Cargo cannot create a second command
alias from one binary target; users who want the shorter spelling can add a
\`codew\` symlink to that installed executable. The npm, Homebrew, archive,
shell-installer, and container channels install both command names directly.

### Manual download — platform archives (recommended)

Each archive below contains the same runtime under the \`codewhale\` and
\`codew\` command names, plus an install script:

| Platform | Archive | Install script |
|---|---|---|
| Linux x64 | \`codewhale-linux-x64.tar.gz\` | \`install.sh\` |
| Linux ARM64 | \`codewhale-linux-arm64.tar.gz\` | \`install.sh\` |
| Android ARM64 (Termux) | \`codewhale-android-arm64.tar.gz\` | \`install.sh\` |
| HarmonyOS ARM64 (native) | \`codewhale-ohos-arm64.tar.gz\` | \`install.sh\` |
| macOS x64 | \`codewhale-macos-x64.tar.gz\` | \`install.sh\` |
| macOS ARM | \`codewhale-macos-arm64.tar.gz\` | \`install.sh\` |
| Windows x64 (installer) | \`CodeWhaleSetup.exe\` | NSIS setup |
| Windows x64 | \`codewhale-windows-x64.zip\` | \`install.bat\` |
| Windows x64 (portable) | \`codewhale-windows-x64-portable.zip\` | — |
| Windows ARM64 | \`codewhale-windows-arm64.zip\` | \`install.bat\` |
| Windows ARM64 (portable) | \`codewhale-windows-arm64-portable.zip\` | — |

**Unix (Linux / macOS):**
\`\`\`bash
tar xzf codewhale-<platform>.tar.gz
cd codewhale-<platform>
./install.sh
\`\`\`

**Windows:**
- For the installer path, run \`CodeWhaleSetup.exe\`; it installs
  \`codewhale.exe\`, \`codew.exe\`, and \`codewhale.bat\` under
  \`%LOCALAPPDATA%\\Programs\\CodeWhale\\bin\`, adds that directory to the
  current-user PATH, and creates a Start Menu shortcut that prefers
  Windows Terminal (\`wt.exe\`) when it is installed.
- Extract the archive for your machine: \`codewhale-windows-x64.zip\` or
  \`codewhale-windows-arm64.zip\`
- Double-click \`codewhale.bat\` (not the raw \`.exe\`) to launch
- Run \`install.bat\` to copy the binaries and launcher to \`%USERPROFILE%\\bin\`
- Add \`%USERPROFILE%\\bin\` to your PATH

The **portable** Windows archive skips the install script — extract and run \`codewhale.bat\` from any directory. The NSIS installer is currently unsigned and may trigger Windows SmartScreen until a signing certificate is wired into the release pipeline.

Each platform also has **bare, unarchived** \`codewhale-<platform>\` and
\`codew-<platform>\` assets. The seven \`codewhale-tui-<platform>\` filenames
attached to v0.9.5 are byte-identical compatibility copies used only to let
already-installed v0.9.4 clients discover and cross this single-binary
transition; current installers do not expose a third runtime. The legacy npm
package \`deepseek-tui\` is deprecated and is not republished. For migration
from v0.8.x legacy binary names, see \`docs/REBRAND.md\`.

### Verify (recommended)

Download the checksum manifests from this Release and verify:

\`\`\`bash
# Linux — archive bundles
sha256sum -c codewhale-bundles-sha256.txt --ignore-missing

# Linux — individual binaries
sha256sum -c codewhale-artifacts-sha256.txt --ignore-missing

# macOS
shasum -a 256 -c codewhale-bundles-sha256.txt --ignore-missing
shasum -a 256 -c codewhale-artifacts-sha256.txt --ignore-missing
\`\`\`

EOF

if [[ -n "${details}" ]]; then
  printf '%s\n\n' '<details>' '<summary>Full release notes</summary>'
  printf '%s\n\n' "${details}" '</details>'
fi

cat <<EOF

## Contributors
EOF

if [[ -n "${contributors}" ]]; then
  printf '%s\n' "${contributors}"
else
  printf '%s\n' "Thank you to everyone whose reports, PRs, reviews, and reproductions shaped this release."
fi

cat <<EOF

See [CHANGELOG.md](https://github.com/codewhale-hq/CodeWhale/blob/main/CHANGELOG.md) for full notes and [docs/CHANGELOG_ARCHIVE.md](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/CHANGELOG_ARCHIVE.md) for older releases.
EOF
}

body="$(render_body)"
if (( ${#body} >= 125000 )); then
  details="The full notes exceed GitHub's release-description limit. Read the [complete changelog for ${tag}](https://github.com/codewhale-hq/CodeWhale/blob/${tag}/CHANGELOG.md)."
  body="$(render_body)"
fi
if (( ${#body} >= 125000 )); then
  echo "Release summary, installation instructions and contributor credits exceed GitHub's 125000-character limit." >&2
  exit 1
fi
printf '%s\n' "${body}"
