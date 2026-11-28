// Markdown, drawn safely.
//
// What an agent wrote is shown as text with markdown formatting, never as the
// HTML it may contain: a `<` or `&` the author typed is a character
// (`literal`), the result is run through DOMPurify with a short list of tags, no
// images, no styles and links limited to http(s), and every link opens in a new
// tab without a referrer. The person's own words keep their line breaks
// (`typed`). The escaping rules are the Herdr window's (`lib/chat.js`).

import DOMPurify from "dompurify";
import { Marked } from "marked";

/** The tags a message may use; everything else is removed, its text kept. */
const TAGS = [
  "p", "br", "hr", "strong", "em", "del", "code", "pre", "blockquote",
  "ul", "ol", "li", "a", "h1", "h2", "h3", "h4", "h5", "h6",
  "table", "thead", "tbody", "tr", "th", "td", "span",
];

/** Whether a link may be opened: web pages only. */
export function safeLink(url: string): boolean {
  return /^https?:\/\/[^\s]+$/i.test(url.trim());
}

/**
 * Words as markdown that shows them as written: a `<` or an `&` the author
 * typed is a character, not markup. Code, fenced or inline, is left alone.
 */
export function literal(text: string): string {
  return text
    .split(/(```[\s\S]*?(?:```|$)|`[^`\n]*`)/)
    .map((part, i) => (i % 2 === 1 ? part : part.replace(/</g, "\\<").replace(/&(?=#?\w+;)/g, "\\&")))
    .join("");
}

/** The person's own words: line breaks they typed stay breaks, and markup stays text. */
export function typed(text: string): string {
  return literal(
    text
      .split(/(```[\s\S]*?(?:```|$))/)
      .map((part, i) => (i % 2 === 1 ? part : part.replace(/([^\n])\n(?!\n)/g, "$1  \n")))
      .join(""),
  );
}

const marked = new Marked({ gfm: true, async: false });

let hooked = false;

/** Links open elsewhere and leak nothing; a link that is not http(s) loses its address. */
function hook(): void {
  if (hooked) {
    return;
  }
  hooked = true;
  DOMPurify.addHook("afterSanitizeAttributes", (node) => {
    if (node.tagName === "A") {
      const href = node.getAttribute("href") ?? "";
      if (!safeLink(href)) {
        node.removeAttribute("href");
        return;
      }
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer nofollow");
    }
  });
}

/** The safe HTML of one message. `fromPerson` keeps the person's line breaks. */
export function renderMarkdown(text: string, fromPerson = false): string {
  hook();
  const source = fromPerson ? typed(text) : literal(text);
  const html = marked.parse(source) as string;
  return DOMPurify.sanitize(html, {
    ALLOWED_TAGS: TAGS,
    ALLOWED_ATTR: ["href", "title"],
    ALLOW_DATA_ATTR: false,
    ALLOW_ARIA_ATTR: false,
  });
}
