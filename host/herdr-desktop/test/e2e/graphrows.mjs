// What the Graph view draws for a live /api/state (read from stdin): the same
// `graphRows` the pane calls, so the e2e run can say what the pane shows.
import { readFileSync } from "node:fs";
import { graphRows } from "../../mods/lib/tree.js";

const state = JSON.parse(readFileSync(0, "utf8"));
const rows = graphRows(state, null, []);
console.log(JSON.stringify(rows.map((r) => ({ kind: r.kind, id: r.id ?? r.task?.id, title: r.title ?? r.task?.title, waits: r.waits ?? r.after ?? null, status: r.task?.derived?.status ?? r.status })).slice(0, 30)));
