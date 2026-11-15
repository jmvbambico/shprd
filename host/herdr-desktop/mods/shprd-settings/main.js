// The settings: agents, roles, custom roles and the prompts each role starts with.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/shepherdr-settings/main.js` (its eight pages, its
//   *Applies to: everywhere* switch, and its write-with-no-Save-step rule)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// shprd's settings are layered — your own file, this repository's entry, the
// repository's committed defaults, and what shprd ships — and the service prints
// every value with the layer it came from. M10.6 fills the page over
// `GET /api/settings` and `PUT /api/settings`, which the client already carries
// (`settings()`, `putSetting()`) with the prompt routes beside them
// (`prompt()`, `putPrompt()`).
//
// M10.3 ships the block so the layout passes `herdr-app check`.

import { View } from "gpui-kit";
import { theme } from "herdr";

import { placeholder } from "./lib/panel.js";

export default class Settings extends View {
  render() {
    return placeholder(
      theme(),
      "Settings",
      "Roles, agents and the role prompts land here in M10.6, each value with its layer.",
      "M10.6",
    );
  }
}
