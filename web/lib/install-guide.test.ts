import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { buildInstallGuide, installAnchorErrors, renderInstallGuideModule } from "../scripts/install-guide-lib.mjs";
import { CHINESE_INSTALL_GUIDE, INSTALL_GUIDE } from "./install-guide.generated";

describe("canonical installation guide", () => {
  it("ships both current source guides with their historical release receipts", () => {
    const source = readFileSync(new URL("../../docs/INSTALL.md", import.meta.url), "utf8");
    const chineseSource = readFileSync(new URL("../../docs/zh_hans/INSTALL.md", import.meta.url), "utf8");
    expect(readFileSync(new URL("./install-guide.generated.ts", import.meta.url), "utf8"))
      .toBe(renderInstallGuideModule(source, chineseSource));
    expect(INSTALL_GUIDE.chunks.map((chunk) => chunk.text).join("\n"))
      .toContain("0.10.0");
    expect(CHINESE_INSTALL_GUIDE.chunks.map((chunk) => chunk.text).join("\n"))
      .toContain("使用当前源码参与测试与贡献");
    expect(CHINESE_INSTALL_GUIDE.sourceHash).not.toBe(INSTALL_GUIDE.sourceHash);
  });

  it("keeps top-level command bytes intact and nested code inside its list", () => {
    const guide = buildInstallGuide('# Setup\n\n```sh\nprintf "$HOME & <path>"\n```\n\n1. In a list:\n\n   ```sh\n   echo nested\n   ```\n');
    expect(guide.chunks[1]).toEqual({ kind: "code", text: 'printf "$HOME & <path>"', lang: "sh" });
    expect(guide.chunks[2].text).toMatch(/<ol>[\s\S]*<li>[\s\S]*<pre><code[\s\S]*echo nested[\s\S]*<\/li>[\s\S]*<\/ol>/);
  });

  it("resolves relative docs and preserves duplicate-heading and legacy anchors", () => {
    const guide = buildInstallGuide('# Start\n\n## Notes\n\n## Notes\n\n<a id="legacy"></a>\n\n[Keys](KEYBINDINGS.md) [Notes](INSTALL.md#notes-1) [Old](#legacy)\n');
    expect(guide.anchors).toEqual(["start", "notes", "notes-1", "legacy"]);
    expect(guide.chunks[0].text).toContain('href="https://github.com/codewhale-hq/CodeWhale/blob/main/docs/KEYBINDINGS.md"');
    expect(guide.chunks[0].text).toContain('href="#notes-1"');
    expect(installAnchorErrors("INSTALL.md#notes-1 INSTALL.md#missing", guide.anchors)).toEqual(["missing"]);
  });

  it("fails generation on a broken internal link", () => {
    expect(() => buildInstallGuide("# Setup\n\n[Missing](#gone)"))
      .toThrow("Missing INSTALL.md anchors: gone");
  });

  it("keeps Chinese anchors and relative source links in the Chinese document", () => {
    const guide = buildInstallGuide('# 安装\n\n<a id="通过-cargo-安装"></a>\n\n[镜像](CNB_MIRROR.md) [英文](../INSTALL.md) [源码](INSTALL.md#通过-cargo-安装)\n', { sourcePath: "zh_hans/INSTALL.md" });
    const html = guide.chunks.map((chunk) => chunk.text).join("\n");
    expect(guide.anchors).toEqual(["安装", "通过-cargo-安装"]);
    expect(html).toContain('href="https://github.com/codewhale-hq/CodeWhale/blob/main/docs/zh_hans/CNB_MIRROR.md"');
    expect(html).toContain('href="https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md"');
    expect(html).toContain('href="#%E9%80%9A%E8%BF%87-cargo-%E5%AE%89%E8%A3%85"');
  });

  it("rejects broken Chinese fragments and attributes on a Unicode anchor", () => {
    expect(() => buildInstallGuide('# 安装\n\n[缺失](#不存在)', { sourcePath: "zh_hans/INSTALL.md" }))
      .toThrow("Missing INSTALL.md anchors: 不存在");
    expect(() => buildInstallGuide('<a id="安装" onclick="alert(1)"></a>'))
      .toThrow("Unsupported raw HTML");
  });

  it("gives scrollable tables distinct, escaped names from their headers", () => {
    const table = '| "Need" | Reason |\n| --- | --- |\n| curl | Download |\n';
    const guide = buildInstallGuide(`# Setup\n\n${table}\n${table}`);
    const html = guide.chunks.map((chunk) => chunk.text).join("\n");
    const names = [...html.matchAll(/role="region" aria-label="([^"]+)"/g)].map((match) => match[1]);
    expect(names).toEqual([
      "Installation table 1: &quot;Need&quot; / Reason",
      "Installation table 2: &quot;Need&quot; / Reason",
    ]);
  });

  it.each(["javascript:alert(1)", "data:text/html,hello", "file:///tmp/private"])("rejects unsafe URL %s", (href) => {
    expect(() => buildInstallGuide(`# Setup\n\n[link](${href})`)).toThrow("Unsupported install-guide link protocol");
  });

  it("rejects arbitrary raw HTML instead of publishing it", () => {
    expect(() => buildInstallGuide('# Setup\n\n<img src="x" onerror="alert(1)">'))
      .toThrow("Unsupported raw HTML");
  });
});
