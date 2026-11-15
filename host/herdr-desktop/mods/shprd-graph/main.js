// The run's graph: dagr's document, drawn.
//
// Shepherdr has no graph of its own; dagr is the maintainer's own tool, and the
// plan puts it here as a terminal block refreshed on every ledger write (M10.7).
//
// M10.3 ships the block so the layout's stack has its child and the layout passes
// `herdr-app check`. M10.7 fills it: dagr in a terminal block, refreshed on every
// change the service touches.

import { View } from "gpui-kit";
import { theme } from "herdr";

import { placeholder } from "./lib/panel.js";

export default class Graph extends View {
  render() {
    return placeholder(
      theme(),
      "Graph",
      "This block runs dagr in a terminal, refreshed on every ledger write. M10.7.",
      "M10.7",
    );
  }
}
