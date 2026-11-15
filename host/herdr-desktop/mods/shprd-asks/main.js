// The needs-you cards: what an agent asked, and the answer it is waiting for.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat/main.js` (the question, permission, secret and plan cards
//   drawn in the conversation), `core/asks.js` (the queue's four folders)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// shprd keeps the same idea in one place: an ask is a file the program's hook
// raises and one answer (`crates/shprd-core/src/ask.rs`), and the service hands
// the open ones to a client at `GET /api/asks` and takes an answer at
// `POST /api/asks/answer`. The client for both is `lib/client.js` — `asks()` and
// `answerAsk()` — so M10.6 fills this panel without a new transport.
//
// M10.3 ships the block so the layout passes `herdr-app check`.

import { View } from "gpui-kit";
import { theme } from "herdr";

import { placeholder } from "./lib/panel.js";

export default class Asks extends View {
  render() {
    return placeholder(
      theme(),
      "Needs you",
      "The open cards land here in M10.6: the question, its options and one answer.",
      "M10.6",
    );
  }
}
