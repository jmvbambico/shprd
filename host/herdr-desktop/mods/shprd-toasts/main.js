// The toasts: Herdr's notifications and shprd's own, floated over the work area.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/toasts/main.js` (up to three, 360 px wide, a dot per level, a
//   one-line title, a body capped at three lines, hover pauses expiry), and
//   `core/toasts.js` for those numbers
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// It is mounted as a floating block over the work area, and it renders nothing
// until there is something to say: a float sizes to its content, so an empty
// render is how it hides. M10.4 fills it; M10.3 ships the empty float so the
// layout's `floating` entry names a mod that exists and `herdr-app check`
// passes.
//
// Until then Herdr's own toasts still reach the person: the app draws them when
// no layout names a replacement, which is what `toast` in `settings.json` does.

import { View } from "gpui-kit";

export default class Toasts extends View {
  render() {
    // Nothing to draw: the app's own toasts are still the ones a person sees.
    return "";
  }
}
