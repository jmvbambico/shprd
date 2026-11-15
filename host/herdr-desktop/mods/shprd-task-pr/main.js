// The task's pull request: its state, its checks and the link.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/task-pr/main.js`
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// M10.3 ships the block so the layout's `tasks-thread-body` stack has its child
// and the layout passes `herdr-app check`. M10.5 fills it from the task's own
// record and its landing.

import { View } from "gpui-kit";
import { theme } from "herdr";

import { placeholder } from "./lib/panel.js";

export default class TaskPr extends View {
  render() {
    return placeholder(
      theme(),
      "Pull request",
      "The task's branch, its checks and its link land here in M10.5.",
      "M10.5",
    );
  }
}
