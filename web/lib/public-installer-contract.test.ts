import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";

const installer = fileURLToPath(new URL("../public/install.sh", import.meta.url));
const fixtureRoots: string[] = [];

function executable(contents: string): Buffer {
  return Buffer.from(`#!/bin/sh\nprintf '%s\\n' '${contents}'\n`, "utf8");
}

function sha256(contents: Buffer): string {
  return createHash("sha256").update(contents).digest("hex");
}

function installFixture(
  withLegacyTui: boolean,
  options: {
    os?: "Darwin" | "Linux";
    arch?: "x86_64" | "aarch64";
    version?: string;
    glibc?: string;
    expectedStatus?: number;
    shell?: string;
    installUnderHome?: boolean;
    installDirName?: string;
    homeFiles?: string[];
    onPath?: "first" | "shadowed";
  } = {},
) {
  const {
    os = "Darwin",
    arch = "x86_64",
    version,
    glibc = "2.39",
    expectedStatus = 0,
    shell = "/bin/zsh",
    installUnderHome = false,
    installDirName = "install",
    homeFiles = [],
    onPath,
  } = options;
  const root = mkdtempSync(path.join(tmpdir(), "codewhale-web-installer-"));
  fixtureRoots.push(root);
  const releaseDir = path.join(root, "release");
  const home = path.join(root, "home");
  const installDir = installUnderHome
    ? path.join(home, ".local", "bin")
    : path.join(root, installDirName);
  const fakeBin = path.join(root, "fake-bin");
  mkdirSync(releaseDir, { recursive: true });
  mkdirSync(installDir, { recursive: true });
  mkdirSync(fakeBin, { recursive: true });

  const runtime = executable("codewhale 0.9.5 (fixture)");
  const target = `${os === "Darwin" ? "macos" : "linux"}-${arch === "aarch64" ? "arm64" : "x64"}`;
  const assets = [`codewhale-${target}`, `codew-${target}`];
  for (const asset of assets) {
    writeFileSync(path.join(releaseDir, asset), runtime);
  }
  writeFileSync(
    path.join(releaseDir, "codewhale-artifacts-sha256.txt"),
    assets.map((asset) => `${sha256(runtime)}  ${asset}`).join("\n") + "\n",
  );

  const fakeUname = [
    "#!/bin/sh",
    'case "${1:-}" in',
    `  -s) printf '%s\\n' ${os} ;;`,
    `  -m) printf '%s\\n' ${arch} ;;`,
    `  *) printf '%s\\n' ${os} ;;`,
    "esac",
    "",
  ].join("\n");
  const fakeCurl = [
    "#!/bin/sh",
    'out=""',
    'url=""',
    'while [ "$#" -gt 0 ]; do',
    '  case "$1" in',
    '    -o) shift; out="$1" ;;',
    "    -*) ;;",
    '    *) url="$1" ;;',
    "  esac",
    "  shift",
    "done",
    'cp "$FAKE_RELEASE_DIR/${url##*/}" "$out"',
    "",
  ].join("\n");
  const fakeGetconf = [
    "#!/bin/sh",
    `printf '%s\\n' 'glibc ${glibc}'`,
    "",
  ].join("\n");
  for (const [name, contents] of [
    ["uname", fakeUname],
    ["curl", fakeCurl],
    ["getconf", fakeGetconf],
  ]) {
    const destination = path.join(fakeBin, name);
    writeFileSync(destination, contents);
    chmodSync(destination, 0o755);
  }

  for (const file of homeFiles) {
    mkdirSync(home, { recursive: true });
    writeFileSync(path.join(home, file), "# existing profile\n");
  }
  // The installer resolves its directory with `cd -P`, so PATH must carry the
  // real path (macOS tmpdir sits behind a /var -> /private/var symlink).
  const pathPrefix: string[] = [];
  if (onPath === "shadowed") {
    const shadow = path.join(root, "shadow");
    mkdirSync(shadow, { recursive: true });
    for (const name of ["codewhale", "codew"]) {
      writeFileSync(path.join(shadow, name), executable(`${name} other`));
      chmodSync(path.join(shadow, name), 0o755);
    }
    pathPrefix.push(realpathSync(shadow));
  }
  if (onPath) pathPrefix.push(realpathSync(installDir));

  const legacyPath = path.join(installDir, "codewhale-tui");
  if (withLegacyTui) {
    writeFileSync(legacyPath, executable("codewhale-tui 0.9.4 (legacy fixture)"));
    chmodSync(legacyPath, 0o755);
  }

  const result = spawnSync("/bin/sh", [installer], {
    encoding: "utf8",
    env: {
      ...process.env,
      CODEWHALE_INSTALL_DIR: installDir,
      CODEWHALE_RELEASE_BASE_URL: "https://fixtures.invalid/download",
      ...(version ? { CODEWHALE_VERSION: version } : {}),
      FAKE_RELEASE_DIR: releaseDir,
      HOME: home,
      SHELL: shell,
      PATH: [...pathPrefix, fakeBin, process.env.PATH ?? "/usr/bin:/bin"].join(":"),
    },
  });
  expect(result.status, `${result.stdout}\n${result.stderr}`).toBe(expectedStatus);

  return { installDir, legacyPath, result, runtime };
}

afterEach(() => {
  for (const root of fixtureRoots.splice(0)) {
    rmSync(root, { recursive: true, force: true });
  }
});

describe.skipIf(process.platform === "win32")("public installer compatibility contract", () => {
  it("installs beside a leftover legacy TUI command and leaves it untouched", () => {
    const { installDir, legacyPath, runtime } = installFixture(true);

    expect(readFileSync(path.join(installDir, "codewhale"))).toEqual(runtime);
    expect(readFileSync(path.join(installDir, "codew"))).toEqual(runtime);
    expect(readFileSync(legacyPath)).toEqual(executable("codewhale-tui 0.9.4 (legacy fixture)"));
  });

  it("does not create the retired TUI command for a clean v0.9.5 install", () => {
    const { installDir, legacyPath, result, runtime } = installFixture(false);

    expect(readFileSync(path.join(installDir, "codewhale"))).toEqual(runtime);
    expect(readFileSync(path.join(installDir, "codew"))).toEqual(runtime);
    expect(existsSync(legacyPath)).toBe(false);
    expect(result.stdout).not.toContain("Refreshed legacy compatibility command:");
  });

  it.each([
    ["/bin/zsh", "Linux", `printf '%s\\n' '# >>> codewhale PATH >>>' 'export PATH="INSTALL:$PATH"' '# <<< codewhale PATH <<<' >> ~/.zshrc`, ". ~/.zshrc"],
    ["/bin/bash", "Linux", `printf '%s\\n' '# >>> codewhale PATH >>>' 'export PATH="INSTALL:$PATH"' '# <<< codewhale PATH <<<' >> ~/.bashrc`, ". ~/.bashrc"],
    ["/bin/bash", "Darwin", `printf '%s\\n' '# >>> codewhale PATH >>>' 'export PATH="INSTALL:$PATH"' '# <<< codewhale PATH <<<' >> ~/.bash_profile`, ". ~/.bash_profile"],
    ["/usr/bin/fish", "Linux", `fish_add_path "INSTALL"`, "this fish shell"],
    ["/bin/dash", "Linux", `printf '%s\\n' '# >>> codewhale PATH >>>' 'export PATH="INSTALL:$PATH"' '# <<< codewhale PATH <<<' >> ~/.profile`, ". ~/.profile"],
  ] as const)(
    "prints the persistent PATH line for %s on %s without editing a profile",
    (shell, os, persist, reload) => {
      const { installDir, result } = installFixture(false, { shell, os });

      // The fixture install dir is never on PATH, so the hint always prints.
      expect(result.stdout).toContain("PATH selects");
      expect(result.stdout).toContain(persist.replace("INSTALL", realpathSync(installDir)));
      expect(result.stdout).toContain(reload);
      expect(result.stdout).toContain("docs/INSTALL.md#put-it-on-your-path");
      // HOME (a sibling of the install dir) is never created: no profile was written.
      expect(existsSync(path.join(installDir, "..", "home"))).toBe(false);
    },
  );

  it.skipIf(!existsSync("/bin/dash"))("the printed profile and reload commands work in dash", () => {
    const { installDir, result } = installFixture(false, { shell: "/bin/dash", os: "Linux" });
    const persist = result.stdout.split("\n").find((line) => line.trim().startsWith("printf '%s\\n' '# >>> codewhale PATH >>>'"));
    const reload = result.stdout.match(/Then run: (.+?)   \(or open a new terminal\)/)?.[1];
    expect(persist).toBeDefined();
    expect(reload).toBeDefined();
    const home = path.join(installDir, "..", "home");
    mkdirSync(home, { recursive: true });
    const applied = spawnSync("/bin/dash", ["-c", `set -e\n${persist}\n${reload}\ncommand -v codewhale`], {
      encoding: "utf8",
      env: { ...process.env, HOME: home },
    });
    expect(applied.status, applied.stderr).toBe(0);
    expect(applied.stdout.trim()).toBe(path.join(realpathSync(installDir), "codewhale"));
  });

  it("writes the default directory as $HOME/.local/bin in the persistent line", () => {
    const { result } = installFixture(false, { installUnderHome: true });

    expect(result.stdout).toContain(
      `printf '%s\\n' '# >>> codewhale PATH >>>' 'export PATH="$HOME/.local/bin:$PATH"' '# <<< codewhale PATH <<<' >> ~/.zshrc`,
    );
  });

  it.each([
    [[".profile"], "~/.profile"],
    [[".bash_login", ".profile"], "~/.bash_login"],
    [[".bash_profile", ".profile"], "~/.bash_profile"],
  ] as const)("keeps macOS login bash reading %j by naming %s", (homeFiles, profile) => {
    const { result } = installFixture(false, { shell: "/bin/bash", os: "Darwin", homeFiles: [...homeFiles] });

    expect(result.stdout).toContain(`'# <<< codewhale PATH <<<' >> ${profile}\n`);
  });

  it.each(["/bin/tcsh", "/usr/bin/nu"])("prints no POSIX line for the unrecognised shell %s", (shell) => {
    const { result } = installFixture(false, { shell });

    expect(result.stdout).toContain(`this installer has no PATH line for ${path.basename(shell)}`);
    expect(result.stdout).not.toContain("export PATH=");
    expect(result.stdout).toContain("docs/INSTALL.md#put-it-on-your-path");
  });

  it.each(["it's", "$(id)", "a`b`", 'q"x', "back\\slash"])(
    "prints no pasteable PATH line for a directory named %s",
    (installDirName) => {
      const { result } = installFixture(false, { installDirName });

      expect(result.stdout).toContain("contains shell-special characters");
      expect(result.stdout).not.toContain("export PATH=");
      expect(result.stdout).not.toContain(">> ~/");
    },
  );

  it("prints no PATH hint when PATH already selects this install", () => {
    const { result } = installFixture(false, { onPath: "first" });

    expect(result.stdout).not.toContain("PATH selects");
    expect(result.stdout).not.toContain("first on PATH in future shells");
    expect(result.stdout).not.toContain("export PATH=");
  });

  it("prints the PATH hint when an earlier PATH entry shadows this install", () => {
    const { result } = installFixture(false, { onPath: "shadowed" });

    expect(result.stdout).toMatch(/PATH selects .*shadow\/codewhale; this install is/);
    expect(result.stdout).toContain("first on PATH in future shells");
  });

  it.each([undefined, "v0.9.6", "v0.9.11"])(
    "does not apply a glibc floor to static Linux arm64 version %s",
    (version) => {
      const { installDir } = installFixture(false, {
        os: "Linux",
        arch: "aarch64",
        version,
        glibc: "2.17",
      });

      expect(existsSync(path.join(installDir, "codewhale"))).toBe(true);
      expect(existsSync(path.join(installDir, "codew"))).toBe(true);
    },
  );

  it("keeps the truthful glibc preflight for an explicitly requested older Linux arm64 release", () => {
    const { result } = installFixture(false, {
      os: "Linux",
      arch: "aarch64",
      version: "v0.9.5",
      glibc: "2.35",
      expectedStatus: 1,
    });

    expect(result.stderr).toContain(
      "Codewhale v0.9.5 linux-arm64 assets require glibc 2.39 or newer",
    );
    expect(result.stderr).toContain(
      "Current v0.9.6+ assets are static musl builds",
    );
  });
});
