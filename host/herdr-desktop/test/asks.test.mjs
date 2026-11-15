// Unit tests for the needs-you panel's view model: a card out of an open ask, a
// card out of an escalation, the verb an escalation leaves to run, and the answer
// one takes.
//
//   node --import ./test/boot.mjs --test test/          (node 22+)
//   bun test --preload ./test/boot-bun.mjs test/        (bun)
//
// The fixtures are a real `shprd serve`'s answers, captured from a throwaway run
// (`test/fixtures/needs-state.json`, `needs-asks.json`): a Pi worker's
// `shprd ask raise` left open, and two escalations — a spent review whose card
// names the verb a person still has to run (M10.0), and a `start` step that kept
// failing. Nothing here invents a shape the service does not send.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import {
  KIND,
  SECTION,
  actionFor,
  answerBody,
  cards,
  commands,
  expiry,
  footer,
  freeText,
  namesOpenAsk,
  quoted,
  section,
  tally,
  verbOf,
} from "../mods/lib/asks.js";

/** One fixture, parsed. */
function fixture(name) {
  return JSON.parse(readFileSync(fileURLToPath(new URL(`./fixtures/${name}`, import.meta.url)), "utf8"));
}

/** The snapshot a real `GET /api/state` answered, with escalations on it. */
const STATE = fixture("needs-state.json");
/** The asks a real `GET /api/asks` answered, with one open. */
const ASKS = fixture("needs-asks.json");

/** Every card the panel draws from those two answers. */
const CARDS = cards(STATE, ASKS);

/** The card about `task`, by the kind of card it is. */
function card(task, from) {
  return CARDS.find((entry) => entry.task === task && entry.from === from);
}

// -- a card from an ask ----------------------------------------------------

test("an open ask is one card: its question, its choices and its task", () => {
  const ask = ASKS.asks[0];
  const shown = card(ask.task, "ask");
  assert.equal(shown.id, `ask:${ask.task}:${ask.id}`);
  assert.equal(shown.kind, "question");
  assert.equal(shown.title, KIND.question);
  assert.equal(shown.task, ask.task);
  assert.equal(shown.ask, ask.id);
  assert.match(shown.text, /quickstart/);
  assert.deepEqual(shown.options, [
    { label: "CLI only", answer: "CLI only" },
    { label: "Both", answer: "Both" },
    { label: "Leave it to me", answer: "Leave it to me" },
  ]);
  assert.equal(shown.freeText, true, "a question always takes words");
  assert.equal(shown.verb, null);
  assert.equal(shown.action, null);
});

test("a card's footer says what happens if nobody answers", () => {
  const ask = ASKS.asks[0];
  const shown = card(ask.task, "ask");
  // A hook times out, so the card says how long is left rather than what happens.
  assert.match(footer(shown, ask.created_at_ms), /left$/);
  assert.equal(footer(shown, ask.expires_at_ms + 1), "expired");
  assert.equal(expiry(undefined), "");
  assert.equal(expiry(ask.created_at_ms + 30_000, ask.created_at_ms), "under a minute left");
  assert.equal(expiry(ask.created_at_ms + 90_000, ask.created_at_ms), "1m left");
  assert.equal(expiry(ask.created_at_ms + 3 * 3_600_000, ask.created_at_ms), "3h left");
});

test("a permission with choices is answered by a button, not by words", () => {
  const asks = {
    asks: [
      {
        id: "1",
        task: "T1",
        kind: "permission",
        text: "Run `cargo test`?",
        options: ["Allow", "Deny"],
        created_at_ms: 1,
      },
    ],
  };
  const shown = cards({ needs: [] }, asks)[0];
  assert.deepEqual(shown.options.map((option) => option.answer), ["Allow", "Deny"]);
  assert.equal(shown.freeText, false);
  // A choice-less one is answered in words.
  assert.equal(freeText("permission", []), true);
  assert.equal(freeText("plan", ["Approve", "Deny"]), false);
  assert.equal(freeText("question", ["A", "B"]), true);
});

// -- a card from an escalation --------------------------------------------

test("an escalation is a card with its words and its default action", () => {
  const need = STATE.needs.find((entry) => entry.seq === 8);
  const shown = cards(STATE, { asks: [] }).find((entry) => entry.id === `need:${need.seq}`);
  assert.equal(shown.from, "escalation");
  assert.equal(shown.task, "T1");
  assert.equal(shown.title, KIND.escalation);
  assert.equal(shown.options.length, 0);
  assert.equal(shown.freeText, false);
  assert.equal(shown.ask, null);
  assert.match(shown.text, /`start` step has failed/);
  assert.equal(footer(shown), need.default_action);
});

test("a spent review's card shows the verb a person still has to run", () => {
  const spent = STATE.needs.find((entry) => entry.task === "T2");
  const shown = card("T2", "escalation");
  assert.equal(shown.verb, "shprd override --task T2 --rule integrate_after_failed_gate --reason …");
  assert.match(shown.text, /you answered `take it as it is`/);
  // The service has no action for `override`, so the card shows the words and
  // offers no button: a button that did nothing would be worse than the verb.
  assert.equal(shown.action, null);
  assert.deepEqual(commands(spent.body), [
    "shprd override --task T2 --rule integrate_after_failed_gate --reason …",
    "shprd land T2",
  ]);
});

test("the verb is only a command, never a choice or an ask id", () => {
  assert.deepEqual(quoted("say `drop it` then run `shprd drop T1 --reason …`"), [
    "drop it",
    "shprd drop T1 --reason …",
  ]);
  assert.deepEqual(commands("the `start` step failed; answer the card `17.9.0`"), []);
  assert.equal(verbOf("no command here"), null);
  assert.equal(verbOf("`shprd land T1`"), "shprd land T1");
});

test("a verb the service has an action for gets a button", () => {
  assert.deepEqual(actionFor("shprd task status T1 --msg halfway"), {
    kind: "task.status",
    task: "T1",
    msg: "halfway",
  });
  assert.deepEqual(actionFor("shprd task status T1 --msg \"quarter of the way\""), {
    kind: "task.status",
    task: "T1",
    msg: "quarter of the way",
  });
  // Everything else a card names is a person's own verb on the command line.
  assert.equal(actionFor("shprd drop T1 --reason …"), null);
  assert.equal(actionFor("shprd agent spawn --task T1"), null);
  assert.equal(actionFor("shprd task status"), null);
  assert.equal(actionFor("shprd task status T1"), null);
  assert.equal(actionFor("not a shprd command"), null);
  assert.equal(actionFor(null), null);
});

// -- the whole list --------------------------------------------------------

test("the panel draws the asks that can be answered, then the escalations", () => {
  assert.deepEqual(
    CARDS.map((entry) => entry.from),
    ["ask", "escalation", "escalation"],
  );
  assert.deepEqual(tally(CARDS), { ask: 1, escalation: 2, total: 3 });
  assert.deepEqual(section(CARDS, "ask").length, 1);
  assert.deepEqual(section(CARDS, "escalation").length, 2);
  assert.equal(SECTION.ask, "Waiting for your answer");
  assert.equal(SECTION.escalation, "Needs a decision");
  // A service that has not answered yet is a panel with nothing, not a failure.
  assert.deepEqual(cards(null, null), []);
  assert.deepEqual(cards({ tasks: [] }, { asks: [] }), []);
});

test("an escalation naming an open ask is the same card as that ask", () => {
  const ask = ASKS.asks[0];
  const duplicated = {
    ...STATE,
    needs: [
      {
        task: ask.task,
        body: `${ask.task}: its review failed; answer the card \`${ask.id}\`: retry, take it or drop it`,
        default_action: "nothing",
        seq: 99,
      },
    ],
  };
  const shown = cards(duplicated, ASKS);
  assert.equal(shown.length, 1, "one decision, one card");
  assert.equal(shown[0].from, "ask");
  assert.equal(namesOpenAsk(`answer the card \`${ask.id}\``, new Set([ask.id])), true);
  assert.equal(namesOpenAsk("answer the card `other`", new Set([ask.id])), false);
  // Once the ask is answered, the same escalation is the card.
  const answered = cards(duplicated, { asks: [] });
  assert.equal(answered.length, 1);
  assert.equal(answered[0].from, "escalation");
});

// -- answering -------------------------------------------------------------

test("an ask is answered with its task, its id and the person's words", () => {
  const shown = card(ASKS.asks[0].task, "ask");
  assert.deepEqual(answerBody(shown, "  Both  "), {
    task: shown.task,
    ask: shown.ask,
    answer: "Both",
  });
  assert.equal(answerBody(shown, "   "), null, "an empty answer is no answer");
  assert.equal(answerBody(shown, null), null);
  // An escalation has no ask behind it, so there is nothing to post.
  assert.equal(answerBody(card("T2", "escalation"), "drop it"), null);
});

test("a card about no task names the run instead of a blank reference", () => {
  const shown = cards(
    {
      needs: [
        {
          body: "no run yet: start one with `shprd run start`",
          default_action: "nothing moves",
          seq: 1,
        },
      ],
    },
    { asks: [] },
  )[0];
  assert.equal(shown.task, null, "a card about no task names the run instead");
  assert.equal(shown.title, KIND.escalation);
});
