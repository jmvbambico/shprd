// The chat's pages, its timeline with cards among the messages, and the ask
// cards' reading.

import { describe, expect, it } from "vitest";

import { answerBody, cards, footer, forTask, freeText, verbOf } from "./asks";
import { empty, fold, folding, outgoing, status, timeline } from "./chat";
import { ask, snapshot } from "./fixtures";

const page = (texts: string[], extra = {}) => ({
  space: "T1",
  program: "claude",
  session: "s1",
  messages: texts.map((text, i) => ({ role: i % 2 ? "assistant" : "user", text, tools: [], at_ms: 1000 * (i + 1) })),
  cursor: `c${texts.length}`,
  more: false,
  note: null,
  ...extra,
});

describe("fold", () => {
  it("appends the next page and keeps the cursor", () => {
    const one = fold(empty("T1"), page(["a", "b"]));
    const two = fold(one, page(["c"], { cursor: "c3" }));
    expect(two.messages.map((m) => m.text)).toEqual(["a", "b", "c"]);
    expect(two.cursor).toBe("c3");
  });

  it("starts over when the agent restarted", () => {
    const one = fold(empty("T1"), page(["a", "b"]));
    expect(fold(one, page(["z"], { session: "s2" })).messages.map((m) => m.text)).toEqual(["z"]);
  });

  it("says the service's note, or a calm default, for an empty chat", () => {
    expect(status(empty("T1"))).toBe("The agent's conversation shows here as it writes it.");
    expect(status({ ...empty("T1"), note: "claude has not written a session yet" })).toBe("claude has not written a session yet");
  });
});

describe("timeline", () => {
  it("puts a card before the first message written after it, and the rest at the end", () => {
    const chat = fold(empty("T1"), page(["a", "b", "c"]));
    const state = snapshot([]);
    const list = cards(state, { asks: [ask("T1", "q1", { created_at_ms: 1500 }), ask("T1", "q2", { created_at_ms: 9000 })] });
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

describe("cards", () => {
  it("draws an ask once: an escalation that names an open ask is dropped", () => {
    const state = snapshot([], {
      needs: [
        { task: "T1", body: "Answer the card `q1`, then run `shprd drop T1 --reason x`", default_action: "wait", seq: 1 },
        { task: "T1", body: "Review spent: run `shprd retry T1`", default_action: "", seq: 2 },
      ],
    });
    const list = cards(state, { asks: [ask("T1", "q1", { options: ["yes", "no"], kind: "permission" })] });
    expect(list.map((c) => c.id)).toEqual(["ask:T1:q1", "need:2"]);
    expect(list[1]?.verb).toBe("shprd retry T1");
    expect(forTask(list, "T1")).toHaveLength(2);
    expect(forTask(list, "T2")).toHaveLength(0);
  });

  it("builds the answer body only for an ask with words", () => {
    const [card] = cards(snapshot([]), { asks: [ask("T1", "q1")] });
    expect(answerBody(card!, "  main ")).toEqual({ task: "T1", ask: "q1", answer: "main" });
    expect(answerBody(card!, "  ")).toBeNull();
    const [esc] = cards(snapshot([], { needs: [{ task: "T1", body: "x", default_action: "", seq: 1 }] }), null);
    expect(answerBody(esc!, "y")).toBeNull();
    expect(footer(esc!)).toBe("");
  });

  it("types an answer where the card takes words", () => {
    expect(freeText("question", ["a"])).toBe(true);
    expect(freeText("permission", ["yes", "no"])).toBe(false);
    expect(freeText("plan", [])).toBe(true);
    expect(verbOf("nothing here")).toBeNull();
  });
});

describe("composer and folding", () => {
  it("never sends an empty turn", () => {
    expect(outgoing("T1", "  ")).toBeNull();
    expect(outgoing("T1", " hi ")).toEqual({ space: "T1", text: "hi" });
  });

  it("folds a very tall reply and shows all of it on request", () => {
    const tall = Array.from({ length: 40 }, (_, i) => `line ${i}`).join("\n");
    expect(folding(tall)).toMatchObject({ foldable: true, folded: true });
    expect(folding(tall, true)).toMatchObject({ foldable: true, folded: false });
    expect(folding("short")).toEqual({ text: "short", foldable: false, folded: false });
  });
});
