// @vitest-environment jsdom

import { describe, expect, it } from "vitest";

import { renderMarkdown, safeLink } from "./markdown";

describe("renderMarkdown", () => {
  it("draws markdown", () => {
    const html = renderMarkdown("**bold** and `code`\n\n- one\n- two");
    expect(html).toContain("<strong>bold</strong>");
    expect(html).toContain("<code>code</code>");
    expect(html).toContain("<li>one</li>");
  });

  it("shows markup an agent typed as text, never as markup", () => {
    const html = renderMarkdown('<script>alert(1)</script> <img src=x onerror=alert(1)> & <b>hi</b>');
    expect(html).not.toContain("<script");
    expect(html).not.toContain("<img");
    expect(html).toContain("&lt;script&gt;");
    const dom = document.createElement("div");
    dom.innerHTML = html;
    expect(dom.querySelector("script, img, b")).toBeNull();
    expect(dom.textContent).toContain("<img src=x onerror=alert(1)>");
  });

  it("strips what DOMPurify's list does not allow, even from a code-looking block", () => {
    const html = renderMarkdown("[x](javascript:alert(1)) [ok](https://example.com/a?b=1) ![i](https://e.com/p.png)");
    expect(html).not.toContain("javascript:");
    expect(html).not.toContain("<img");
    expect(html).toContain('href="https://example.com/a?b=1"');
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer nofollow"');
  });

  it("limits links to http(s)", () => {
    expect(safeLink("https://example.com")).toBe(true);
    expect(safeLink("http://example.com/x")).toBe(true);
    expect(safeLink("javascript:alert(1)")).toBe(false);
    expect(safeLink("data:text/html,x")).toBe(false);
    expect(safeLink("file:///etc/passwd")).toBe(false);
    expect(renderMarkdown("[a](ftp://x.example/y)")).not.toContain("href");
  });

  it("keeps the line breaks of the person's own words", () => {
    expect(renderMarkdown("one\ntwo", true)).toContain("<br>");
    expect(renderMarkdown("one\ntwo", false)).not.toContain("<br>");
  });
});
