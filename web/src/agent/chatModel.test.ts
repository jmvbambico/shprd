import { describe, expect, it } from "vitest";

import type { ChatDto } from "../gen";
import { ask, snapshot } from "../tasks/fixtures";
import { cards } from "../tasks/asks";
import { COORDINATOR, FOLD, KEEP, empty, fold, folding, outgoing, timeline, toolLine, when } from "./chatModel";

function page(over: Partial<ChatDto> = {}): ChatDto {
  return {
    space: "coordinator",
    program: "claude",
    session: "s1",
    messages: [],
    cursor: "c1",
    more: false,
    older: null,
    note: null,
    question: null,
    ...over,
  };
}

const say = (text: string, role = "assistant") => ({ role, text, tools: [], at_ms: 1 });

describe("fold", () => {
  it("appends what came after the cursor", () => {
    const one = fold(null, page({ messages: [say("a")], cursor: "c1" }));
    const two = fold(one, page({ messages: [say("b")], cursor: "c2" }));
    expect(two.messages.map((m) => m.text)).toEqual(["a", "b"]);
    expect(two.cursor).toBe("c2");
  });

  it("keeps what the service told the agent as news, not as the person's words", () => {
    const held = fold(null, page({ messages: [say("T7 needs you: pick one", "news")] }));
    expect(held.messages[0]?.role).toBe("news");
  });

  it("starts over when the agent began a new session", () => {
    const one = fold(null, page({ messages: [say("old")] }));
    const two = fold(one, page({ session: "s2", messages: [say("new")] }));
    expect(two.messages.map((m) => m.text)).toEqual(["new"]);
  });

  it("keeps the newest KEEP messages", () => {
    const many = Array.from({ length: KEEP + 5 }, (_, i) => say(`m${i}`));
    const held = fold(null, page({ messages: many }));
    expect(held.messages).toHaveLength(KEEP);
    expect(held.messages[0]?.text).toBe("m5");
  });

  it("keeps the note of the last page, and the program once known", () => {
    const one = fold(null, page({ program: "claude", messages: [say("a")] }));
    const two = fold(one, page({ program: null, note: "nothing new" }));
    expect(two.program).toBe("claude");
    expect(two.note).toBe("nothing new");
  });
});

describe("outgoing", () => {
  it("trims, and sends nothing for nothing", () => {
    expect(outgoing("T1", "  hi  ")).toEqual({ space: "T1", text: "hi" });
    expect(outgoing("T1", "  \n ")).toBeNull();
    expect(outgoing("", "hi")).toEqual({ space: COORDINATOR, text: "hi" });
  });
});

describe("folding", () => {
  it("never folds a short reply", () => {
    expect(folding("short")).toEqual({ text: "short", foldable: false, folded: false });
  });

  it("folds a tall reply to its first lines and opens on request", () => {
    const tall = Array.from({ length: 40 }, (_, i) => `line ${i}`).join("\n");
    const folded = folding(tall);
    expect(folded.folded).toBe(true);
    expect(folded.text.split("\n")).toHaveLength(FOLD);
    expect(folding(tall, true)).toEqual({ text: tall, foldable: true, folded: false });
  });
});

describe("words", () => {
  it("states a tool call and a time", () => {
    expect(toolLine({ name: "Bash", summary: " ls " })).toBe("Bash · ls");
    expect(toolLine({ name: "Read", summary: "" })).toBe("Read");
    expect(when(null)).toBe("");
    expect(when(0)).toBe("");
    expect(when(new Date(2026, 10, 18, 9, 5).getTime())).toBe("09:05");
    expect(empty("T1").space).toBe("T1");
  });
});

describe("timeline", () => {
  it("puts a card before the first message written after it, and the rest at the end", () => {
    const chat = fold(
      null,
      page({ space: "T1", messages: ["a", "b", "c"].map((text, i) => ({ ...say(text), at_ms: 1000 * (i + 1) })) }),
    );
    const list = cards(snapshot([]), { asks: [ask("T1", "q1", { created_at_ms: 1500 }), ask("T1", "q2", { created_at_ms: 9000 })] });
    const rows = timeline(chat, list);
    expect(rows.map((r) => (r.kind === "message" ? r.message.text : r.kind === "card" ? `card:${r.card.ask}` : r.kind))).toEqual([
      "top",
      "a",
      "card:q1",
      "b",
      "c",
      "card:q2",
    ]);
  });
});
