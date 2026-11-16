// Registers the app's module stubs for bun, whose loader hook is `--preload` plus
// `mock.module` rather than node's `register`.
//
//   bun test --preload ./test/boot-bun.mjs test/
//
// A Herdr mod imports `herdr`, `gpui-kit` and `gpui-base` by name; this points
// those three at the stubs and leaves every other import alone.

import { mock } from "bun:test";

import * as herdr from "./stub-herdr.js";
import * as gpui from "./stub-gpui.js";

mock.module("herdr", () => herdr);
mock.module("gpui-kit", () => gpui);
mock.module("gpui-base", () => gpui);
mock.module("gpui", () => gpui);
