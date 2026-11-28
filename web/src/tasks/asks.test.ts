// The ask cards' reading of the service.

import { describe, expect, it } from "vitest";

import { answerBody, cards, footer, forTask, freeText, verbOf } from "./asks";
import { ask, snapshot } from "./fixtures";

describe("cards", () => {
  it("draws an ask once: an escalation that names an open ask is dropped", () => {
    const state = snapshot([], {
      needs: [
        { task: "T1", body: "Answer the card `q1`, then run `shprd drop T1 --reason x`", default_action: "wait", seq: 1, kind: "decision", actions: [], key: "", for_you: true },
        { task: "T1", body: "Review spent: run `shprd retry T1`", default_action: "", seq: 2, kind: "decision", actions: [], key: "", for_you: true },
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
    const [esc] = cards(snapshot([], { needs: [{ task: "T1", body: "x", default_action: "", seq: 1, kind: "decision", actions: [], key: "", for_you: true }] }), null);
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
