import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
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
    args?: string[];
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
    args = [],
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
  mkdirSync(installDir, { recursive: true, mode: 0o700 });
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

  mkdirSync(home, { recursive: true, mode: 0o700 });
  for (const file of homeFiles) {
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

  const env = {
      ...process.env,
      CODEWHALE_INSTALL_DIR: installDir,
      CODEWHALE_RELEASE_BASE_URL: "https://fixtures.invalid/download",
      ...(version ? { CODEWHALE_VERSION: version } : {}),
      FAKE_RELEASE_DIR: releaseDir,
      HOME: home,
      SHELL: shell,
      PATH: [...pathPrefix, fakeBin, process.env.PATH ?? "/usr/bin:/bin"].join(":"),
  };
  const result = spawnSync("/bin/sh", [installer, ...args], { encoding: "utf8", env });
  expect(result.status, `${result.stdout}\n${result.stderr}`).toBe(expectedStatus);

  return { env, installDir, legacyPath, result, runtime };
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

  const block = (dir: string) => `# >>> codewhale PATH >>>\nexport PATH="${dir}:$PATH"\n# <<< codewhale PATH <<<\n`;

  it.each([
    ["/bin/zsh", "Linux", ".zshrc"],
    ["/bin/bash", "Linux", ".bashrc"],
    ["/bin/bash", "Darwin", ".bash_profile"],
    ["/bin/dash", "Linux", ".profile"],
  ] as const)(
    "adds the marked PATH block to the profile for %s on %s and says so",
    (shell, os, profile) => {
      const { installDir, result } = installFixture(false, { shell, os });
      const home = path.join(installDir, "..", "home");

      expect(readFileSync(path.join(home, profile), "utf8")).toBe(block(realpathSync(installDir)));
      expect(result.stdout).toContain(`Added to ~/${profile}`);
      expect(result.stdout).toContain("--no-modify-path");
      expect(result.stdout).toContain("To undo, run:");
      expect(result.stdout).toContain("export PATH=");
      expect(result.stdout).toContain("docs/INSTALL.md#put-it-on-your-path");
    },
  );

  it("prints the fish command and edits no file", () => {
    const { installDir, result } = installFixture(false, { shell: "/usr/bin/fish", os: "Linux" });

    expect(result.stdout).toContain(`fish_add_path "${realpathSync(installDir)}"`);
    expect(result.stdout).toContain("this fish shell");
    expect(readdirSync(path.join(installDir, "..", "home"))).toEqual([]);
  });

  it("skips the edit with --no-modify-path and prints the command instead", () => {
    const { installDir, result } = installFixture(false, { shell: "/bin/zsh", os: "Linux", args: ["--no-modify-path"] });

    expect(readdirSync(path.join(installDir, "..", "home"))).toEqual([]);
    expect(result.stdout).toContain("Not editing your shell profile");
    expect(result.stdout).toContain(`'# <<< codewhale PATH <<<' >> ~/.zshrc`);
  });

  it("never duplicates the block when the installer runs again", () => {
    const first = installFixture(false, { shell: "/bin/zsh", os: "Linux" });
    const home = path.join(first.installDir, "..", "home");
    const before = readFileSync(path.join(home, ".zshrc"), "utf8");
    const again = spawnSync("/bin/sh", [installer], {
      encoding: "utf8",
      env: first.env,
    });

    expect(again.status, again.stderr).toBe(0);
    expect(readFileSync(path.join(home, ".zshrc"), "utf8")).toBe(before);
    expect(again.stdout).toContain("already has the codewhale PATH block");
  });

  it.skipIf(!existsSync("/bin/dash"))("a new dash shell finds codewhale through the added block and the undo command removes it", () => {
    const { installDir, result } = installFixture(false, { shell: "/bin/dash", os: "Linux" });
    const home = path.join(installDir, "..", "home");
    const found = spawnSync("/bin/dash", ["-c", ". \"$HOME/.profile\"; command -v codewhale"], {
      encoding: "utf8",
      env: { ...process.env, HOME: home, PATH: "/usr/bin:/bin" },
    });
    expect(found.status, found.stderr).toBe(0);
    expect(found.stdout.trim()).toBe(path.join(realpathSync(installDir), "codewhale"));

    const undo = result.stdout.split("\n").map((line) => line.trim()).find((line) => line.startsWith("cw_tmp="));
    expect(undo).toBeDefined();
    const undone = spawnSync("/bin/sh", ["-c", undo as string], { encoding: "utf8", env: { ...process.env, HOME: home } });
    expect(undone.status, undone.stderr).toBe(0);
    expect(readFileSync(path.join(home, ".profile"), "utf8")).toBe("");
  });

  it.each([
    ["/bin/zsh", "Linux"],
    ["/bin/bash", "Linux"],
    ["/usr/bin/fish", "Linux"],
    ["/bin/dash", "Darwin"],
  ] as const)("ends with the PATH command and the verify command for %s on %s", (shell, os) => {
    const { result } = installFixture(false, { shell, os, installUnderHome: true });
    const lines = result.stdout.split("\n").map((line) => line.trim()).filter(Boolean);

    expect(lines.at(-1)).toBe("codewhale --version");
    if (shell.endsWith("fish")) {
      expect(lines.at(-5)).toContain("fish_add_path");
    } else {
      expect(lines.at(-3)).toBe('export PATH="$HOME/.local/bin:$PATH"; hash -r');
    }
  });

  it("ends with the full-path verify command for a shell it has no PATH line for", () => {
    const { installDir, result } = installFixture(false, { shell: "/bin/tcsh" });
    const lines = result.stdout.split("\n").map((line) => line.trim()).filter(Boolean);

    expect(lines.at(-1)).toBe(`"${realpathSync(installDir)}/codewhale" --version`);
  });

  it("ends with the next step when PATH already selects this install", () => {
    const { result } = installFixture(false, { onPath: "first" });
    const lines = result.stdout.split("\n").map((line) => line.trim()).filter(Boolean);

    expect(lines.at(-1)).toBe("Next: codewhale --version");
  });

  it("writes the default directory as $HOME/.local/bin in the added block", () => {
    const { installDir } = installFixture(false, { installUnderHome: true });

    expect(readFileSync(path.join(installDir, "..", "..", ".zshrc"), "utf8")).toBe(block("$HOME/.local/bin"));
  });

  it.each([
    [[".profile"], "~/.profile"],
    [[".bash_login", ".profile"], "~/.bash_login"],
    [[".bash_profile", ".profile"], "~/.bash_profile"],
  ] as const)("keeps macOS login bash reading %j by naming %s", (homeFiles, profile) => {
    const { installDir, result } = installFixture(false, { shell: "/bin/bash", os: "Darwin", homeFiles: [...homeFiles] });
    const file = path.join(installDir, "..", "home", profile.slice(2));

    expect(result.stdout).toContain(`Added to ${profile}`);
    expect(readFileSync(file, "utf8")).toBe(`# existing profile\n${block(realpathSync(installDir))}`);
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
    const { installDir, result } = installFixture(false, { onPath: "shadowed" });

    expect(result.stdout).toMatch(/PATH selects .*shadow\/codewhale; this install is/);
    expect(result.stdout).toContain("is on PATH but not first");
    expect(readdirSync(path.join(installDir, "..", "home"))).toEqual([]);
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
