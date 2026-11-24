import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { renderMarkdown } from "../markdown";

describe("renderMarkdown", () => {
  it("escapes HTML so model output cannot inject markup", () => {
    const { html } = renderMarkdown('<script>alert("x")</script>');
    assert.ok(!html.includes("<script>"));
    assert.ok(html.includes("&lt;script&gt;"));
  });

  it("renders fenced code blocks with copy/insert actions", () => {
    const { html, codeBlocks } = renderMarkdown("```ts\nconst x = 1;\nconst y = 2;\n```");
    assert.deepEqual(codeBlocks, ["const x = 1;\nconst y = 2;"]);
    assert.ok(html.includes('class="codeblock"'));
    assert.ok(html.includes(">Copy</button>"));
    assert.ok(html.includes(">Insert</button>"));
    assert.ok(html.includes("codeblock-lang"));
    assert.ok(html.includes("const x = 1;"));
  });

  it("renders consecutive fenced blocks in order", () => {
    const { codeBlocks } = renderMarkdown("```\na\n```\n\n```python\nb\n```");
    assert.deepEqual(codeBlocks, ["a", "b"]);
  });

  it("renders inline code, bold, and headings", () => {
    const { html } = renderMarkdown("## Title\nUse `foo()` and **bold** text.");
    assert.ok(html.includes("<h4>Title</h4>"));
    assert.ok(html.includes("<code>foo()</code>"));
    assert.ok(html.includes("<strong>bold</strong>"));
  });

  it("renders http(s) links only", () => {
    const { html } = renderMarkdown("[site](https://example.com) and [bad](javascript:alert(1))");
    assert.ok(html.includes('<a href="https://example.com">site</a>'));
    assert.ok(!html.includes("href=\"javascript:"));
  });

  it("renders bullet and ordered lists", () => {
    const { html } = renderMarkdown("- one\n- two\n\n1. first\n2. second");
    assert.ok(html.includes("<ul><li>one</li><li>two</li></ul>"));
    assert.ok(html.includes("<ol><li>first</li><li>second</li></ol>"));
  });

  it("does not treat list items inside fenced code as markup", () => {
    const { html, codeBlocks } = renderMarkdown("```\n- not a list\n```");
    assert.deepEqual(codeBlocks, ["- not a list"]);
    assert.ok(!html.includes("<li>"));
  });

  it("escapes double quotes inside code attributes", () => {
    const { html } = renderMarkdown('```\nsay("hi")\n```');
    assert.ok(html.includes("say(&quot;hi&quot;)"));
  });
  it("truncates a code title without splitting a surrogate pair", () => {
    const { html } = renderMarkdown("```\n" + "a".repeat(79) + "😀tail\n```");
    const title = /<code title="([^"]*)"/.exec(html)?.[1] ?? "";
    assert.equal(title, "a".repeat(79) + "😀");
  });
});
