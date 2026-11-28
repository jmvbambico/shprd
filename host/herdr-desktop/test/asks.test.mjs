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
  HEADING,
  KIND,
  SECTION,
  actionFor,
  answerBody,
  cards,
  commands,
  expiry,
  footer,
  freeText,
  ifNothing,
  choiceLabel,
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
  assert.equal(shown.title, HEADING.decision);
  assert.equal(shown.notice, false);
  assert.deepEqual(shown.buttons.map((button) => button.label), ["CLI only", "Both", "Leave it to me"]);
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

test("a card has no grey default or countdown: only a plain default shows, as \"If you do nothing\"", () => {
  const ask = ASKS.asks[0];
  const shown = card(ask.task, "ask");
  assert.equal(footer(shown), "", "an ask says nothing under its words");
  assert.equal(ifNothing("the task stays where it is"), "If you do nothing: the task stays where it is");
  assert.equal(ifNothing("fix it, then the engine retries `start`"), "", "a default with a command in it is the button's job");
  assert.equal(ifNothing(""), "");
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
  assert.equal(shown.title, HEADING.decision);
  assert.equal(shown.options.length, 0);
  assert.equal(shown.freeText, false);
  assert.equal(shown.ask, null);
  assert.match(shown.text, /`start` step has failed/);
  assert.equal(footer(shown), "", "its default names a command, so it is not printed");
  assert.deepEqual(shown.buttons.map((button) => button.label), ["Ask the assistant", "Drop task\u2026"]);
  assert.equal(shown.key, "need:8");
});

test("a spent review's card shows the verb a person still has to run", () => {
  const spent = STATE.needs.find((entry) => entry.task === "T2");
  const shown = card("T2", "escalation");
  // The service sent buttons, so the card is drawn with them and without the raw command.
  assert.equal(shown.verb, null);
  assert.deepEqual(shown.buttons.map((button) => button.label), ["Send back", "Review", "Drop task\u2026"]);
  assert.match(shown.text, /you answered `take it as it is`/);
  assert.equal(shown.footer, undefined);
  assert.equal(footer(shown), "If you do nothing: the task stays where it is");
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
  assert.equal(shown.title, HEADING.decision);
  assert.equal(shown.buttons.length, 0, "a service that sends no buttons gets the card as it was");
});

test("a lost brief's card has a Resend button that posts task.resend_brief", () => {
  const state = { needs: [{ task: "T1", body: "brief not delivered", default_action: "resend the brief", seq: 3 }] };
  const [card] = cards(state, { asks: [] });
  assert.deepEqual(card.action, { kind: "task.resend_brief", task: "T1" });
  assert.equal(card.actionLabel, "Resend");
  assert.equal(footer(card), "If you do nothing: resend the brief");
  assert.equal(card.buttons[0].label, "Resend");
  assert.deepEqual(card.buttons[0].payload, { kind: "task.resend_brief", task: "T1" });
  // A review's lost kickoff is a different card with its own fix: no button.
  const other = cards({ needs: [{ task: "T1", body: "review brief not delivered", default_action: "x", seq: 4 }] }, { asks: [] })[0];
  assert.equal(other.action, null);
});

// -- every card has a button, a notice dismisses, scope cards are one ------

test("a notice is a Notice with Dismiss and no decision, and a decision with no kind reads as a decision", () => {
  const state = {
    needs: [
      { seq: 5, task: "T1", body: "T1 is back in the queue", default_action: "nothing", kind: "notice", actions: ["dismiss"], key: "need:5" },
      { seq: 6, task: "T1", body: "pick one", default_action: "wait", kind: "decision", actions: ["ask_assistant", "dismiss"], key: "need:6" },
      { seq: 7, task: null, body: "an old service's card", default_action: "wait" },
    ],
  };
  const [notice, decision, old] = cards(state, { asks: [] });
  assert.equal(notice.title, HEADING.notice);
  assert.equal(notice.notice, true);
  assert.deepEqual(notice.buttons.map((button) => button.label), ["Dismiss"]);
  assert.deepEqual(notice.buttons[0].payload, { kind: "need.dismiss", seq: 5 });
  assert.equal(footer(notice), "", "a notice has no default");
  assert.equal(decision.title, HEADING.decision);
  assert.equal(footer(decision), "If you do nothing: wait");
  assert.equal(old.title, HEADING.decision);
  assert.equal(old.notice, false);
});

test("asks the service gives one key are one card, and a choice reads as written", () => {
  const asks = {
    asks: [
      { id: "q-1", task: "T1", kind: "question", text: "which format?", options: ["json", "toml"], created_at_ms: 1, key: "k:1" },
      { id: "q-2", task: "T1", kind: "question", text: "which format, again?", options: ["yaml"], created_at_ms: 2, key: "k:1" },
    ],
  };
  const list = cards({ needs: [] }, asks);
  assert.equal(list.length, 1);
  assert.equal(list[0].key, "k:1");
  assert.deepEqual(list[0].buttons.map((button) => button.label), ["json", "toml", "yaml"]);
  assert.deepEqual(list[0].buttons.map((button) => button.ask), ["q-1", "q-1", "q-2"]);
  assert.equal(choiceLabel(" widen the scope to x "), "widen the scope to x");
  // Without a key, every ask is its own card.
  const apart = cards({ needs: [] }, { asks: asks.asks.map(({ key, ...rest }) => rest) });
  assert.equal(apart.length, 2);
});

import { oldestCard } from "../mods/lib/asks.js";

const ASK = (id, task, options = ["yes", "no"]) => ({ id, task, text: `q ${id}`, options, kind: "question", created_at_ms: 1 });

test("oldestCard is the first waiting item, a notice only when nothing else waits", () => {
  assert.equal(oldestCard({ asks: [], needs: [] }), null);
  const state = { asks: [ASK("a1", "T1"), ASK("a2", null)], needs: [] };
  assert.equal(oldestCard(state).task, "T1");
  assert.equal(oldestCard({ asks: [], needs: [{ seq: 1, task: null, body: "plain decision", kind: "decision" }] }).task, null);
});

test("oldestCard reads the open asks list it is given, not only the board's", () => {
  const board = { asks: [], needs: [{ seq: 2, task: "T9", body: "later", kind: "decision" }] };
  assert.equal(oldestCard(board, { asks: [ASK("a1", "T1")] }).task, "T1");
});

import { proposalLabel } from "../mods/lib/asks.js";
import { extraActions, actionBody } from "../mods/lib/board.js";

test("a coordinator proposal's choices read Confirm and Decline, and answer as written", () => {
  const ask = { id: "propose-drop-1", task: "T1", options: ["confirm drop", "leave it"], kind: "question", text: "x\nreason: y" };
  assert.equal(proposalLabel(ask, "confirm drop"), "Confirm");
  assert.equal(proposalLabel(ask, "leave it"), "Decline");
  const card = cards({ asks: [ask], needs: [] }, { asks: [ask] }).find((c) => c.ask === "propose-drop-1");
  assert.deepEqual(card?.buttons.map((b) => [b.label, b.answer]), [["Confirm", "confirm drop"], ["Decline", "leave it"]]);
  // Any other ask keeps its words.
  assert.equal(proposalLabel({ id: "q1" }, "leave it"), "leave it");
});

test("the board offers Reopen, Override and Sync only where they apply", () => {
  const handedIn = { id: "T1", status: "reported", derived: { status: "ready" } };
  assert.ok(extraActions(handedIn).some((a) => a.id === "reopen" && a.action === "task.reopen"));
  const gated = { id: "T2", status: "failed", accept_blocked: "gate lint failed", derived: { status: "needs_you" } };
  assert.ok(extraActions(gated).some((a) => a.id === "override" && a.action === "task.override"));
  assert.ok(!extraActions({ ...gated, accepted_failure: true }).some((a) => a.id === "override"));
  assert.deepEqual(extraActions({ id: "T3", status: "working", derived: { status: "working" } }), []);
  assert.ok(extraActions({ id: "T4", status: "queued", derived: { reason: "behind main: sync" } }).some((a) => a.action === "run.sync"));
});

test("every applicable extra act shows together, and Readmit names a dry agent", () => {
  const task = {
    id: "T1",
    status: "reported",
    derived: { status: "ready", reason: "behind main: sync" },
    attempts: [{ agent: "codex" }, { agent: "pi" }, { agent: "codex" }],
  };
  const state = { agents: [{ id: "codex", dry: true }, { id: "pi", dry: false }] };
  const acts = extraActions(task, state);
  assert.deepEqual(acts.map((a) => a.id), ["reopen", "sync", "readmit-codex"]);
  assert.deepEqual(actionBody(acts[2], task), { kind: "agent.readmit", agent: "codex", reason: "readmitted from the board" });
  assert.deepEqual(actionBody(acts[1], task), { kind: "run.sync" });
  assert.deepEqual(actionBody(acts[0], task), { kind: "task.reopen", task: "T1" });
  assert.ok(!extraActions(task, { agents: [] }).some((a) => a.action === "agent.readmit"));
});

test("a task's decision card offers Readmit for each dry agent, sending its agent and a reason", () => {
  const state = {
    agents: [{ id: "codex", dry: true }, { id: "pi", dry: false }],
    tasks: [{ id: "T1", attempts: [{ agent: "codex" }, { agent: "pi" }] }],
    needs: [{ seq: 5, task: "T1", kind: "escalation", body: "stuck", actions: ["drop"] }],
  };
  const card = cards(state, { asks: [] }).find((c) => c.task === "T1");
  const read = card.buttons.filter((b) => b.action === "readmit");
  assert.equal(read.length, 1);
  assert.equal(read[0].label, "Readmit codex");
  assert.deepEqual(read[0].payload, { kind: "agent.readmit", agent: "codex", reason: "readmitted from its card" });
});

test("an operational run-wide card the coordinator settles is drawn nowhere a person decides", () => {
  const state = {
    needs: [
      { seq: 1, body: "The local `main` has commits the remote lacks: abc fix", default_action: "push", for_you: false, kind: "decision", actions: ["ask_assistant", "dismiss"] },
      { seq: 2, body: "Which database?", default_action: "sqlite", for_you: true, kind: "decision", actions: ["reply", "dismiss"] },
      { seq: 3, body: "an old service's card", default_action: "x", kind: "decision", actions: ["dismiss"] },
    ],
  };
  const drawn = cards(state, { asks: [] });
  assert.deepEqual(drawn.map((card) => card.text), ["Which database?", "an old service's card"]);
});
