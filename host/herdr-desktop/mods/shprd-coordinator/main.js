// The coordinator: the run's planning agent, above the spaces list.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/coordinator/main.js` (its dialog/pane views: `close`, `agents`,
//   `instructions`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// shprd's coordinator is a real CLI in its own space, and M10.8 gives it a chat
// mod reading the agent's own transcript. This block is the coordinator area the
// task asks the layout to carry: M10.3 ships it so the layout passes
// `herdr-app check`, M10.8 fills it.

import { View } from "gpui-kit";
import { theme } from "herdr";

import { placeholder } from "./lib/panel.js";

export default class Coordinator extends View {
  render() {
    return placeholder(
      theme(),
      "Coordinator",
      "The coordinator's conversation lands here in M10.8, read from its own transcript.",
      "M10.8",
    );
  }
}
