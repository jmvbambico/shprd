import { describe, expect, it } from "vitest";

import { maskLink } from "./RemotePage";
import {
  effortChoices,
  health,
  levelNote,
  modelChoices,
  outcomeOf,
  programOf,
  promptRows,
  roleIds,
  roles,
  stepped,
  withRole,
  withoutRole,
  workflowRows,
} from "./model";
import { PROMPTS, ROSTER, row, settings } from "./testkit";

describe("roles", () => {
  it("lists the three built-in roles, even from an empty document", () => {
    expect(roleIds(null)).toEqual(["coordinator", "worker", "reviewer"]);
  });

  it("appends a role whose keys are there but whose id is not in the order", () => {
    const document = settings([row("roles.docs.agent", "claude", "you")]);
    expect(roleIds(document)).toContain("docs");
    expect(roles(document, ROSTER).find((role) => role.id === "docs")?.custom).toBe(true);
  });

  it("reads each pick with the layer that answered it", () => {
    const worker = roles(settings(), ROSTER).find((role) => role.id === "worker");
    const agent = worker?.picks[0];
    expect(agent?.display).toBe("Claude Code");
    expect(agent?.layer).toBe("you");
    expect(worker?.picks[2]?.layer).toBe("nothing sets it");
  });

  it("offers the program's own default first, then its catalog", () => {
    const choices = modelChoices(programOf(ROSTER, "claude"));
    expect(choices[0]?.value).toBeNull();
    expect(choices.map((choice) => choice.value)).toEqual([null, "sonnet", "opus"]);
    expect(choices[1]?.hint).toBe("sonnet");
    expect(choices[2]?.hint).toBeNull();
  });

  it("offers no effort menu for a program that takes none", () => {
    expect(effortChoices(programOf(ROSTER, "codex"), null)).toEqual([]);
    expect(effortChoices(programOf(ROSTER, "claude"), "opus").map((c) => c.value)).toEqual([null, "high"]);
  });

  it("refuses a role id the core would refuse", () => {
    const document = settings();
    expect(withRole(document, "docs")).toEqual(["coordinator", "worker", "reviewer", "docs"]);
    expect(withRole(document, "worker")).toBeNull();
    expect(withRole(document, "order")).toBeNull();
    expect(withRole(document, "no spaces")).toBeNull();
    expect(withoutRole(document, "reviewer")).toEqual(["coordinator", "worker"]);
  });
});

describe("workflow rows", () => {
  it("keeps a key the service does not answer as an unset row", () => {
    const rows = workflowRows(settings());
    const base = rows.find((entry) => entry.key === "tasks.base");
    expect(base?.value).toBeNull();
    expect(base?.display).toBe("not set");
    expect(base?.layer).toBe("nothing sets it");
  });

  it("reads switches, choices and numbers in words", () => {
    const rows = workflowRows(settings());
    expect(rows.find((entry) => entry.key === "tasks.proof")?.display).toBe("On");
    expect(rows.find((entry) => entry.key === "tasks.merge")?.display).toBe("Ask me");
    expect(rows.find((entry) => entry.key === "tasks.archive_after")?.display).toBe("not set");
  });

  it("steps inside the service's own bounds", () => {
    expect(stepped(1, -1, "tasks.review_rounds")).toBe(1);
    expect(stepped(10, 1, "tasks.review_rounds")).toBe(10);
    expect(stepped(null, 1, "tasks.max_running")).toBe(2);
  });
});

describe("the rest", () => {
  it("says what each write did", () => {
    expect(outcomeOf("stored")).toBe("Saved");
    expect(outcomeOf("removed")).toMatch(/inherits again/);
  });

  it("marks an edited and an unreadable prompt", () => {
    const rows = promptRows({
      prompts: [...PROMPTS.prompts, { id: "mine", path: "/p/mine.md", shipped: false, problem: "not UTF-8" }],
    });
    expect(rows.map((entry) => entry.word)).toEqual(["Default", "Edited", "Unreadable"]);
    expect(rows[2]?.about).toBe("a prompt of your own");
  });

  it("separates 'nothing wrong' from 'not checked'", () => {
    expect(health(settings())).toEqual({ problems: [], notChecked: null });
    expect(health(settings([], { not_checked: "no harness data" })).notChecked).toBe("no harness data");
  });

  it("names the repository in the repo scope's note", () => {
    expect(levelNote("repo", "shprd")).toMatch(/shprd/);
    expect(levelNote("you", "shprd")).toMatch(/every repository/);
  });

  it("masks the secret part of a link and keeps where it points", () => {
    const masked = maskLink("https://quiet-river-1234.trycloudflare.com/x?token=abc");
    expect(masked).not.toMatch(/quiet|token|abc/);
    expect(masked.startsWith("https://")).toBe(true);
    expect(maskLink("not a url")).toBe("••••••••");
  });
});
