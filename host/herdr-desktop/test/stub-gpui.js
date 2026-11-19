// The `gpui-kit` and `gpui-base` modules, stubbed for a unit test: element
// builders that record nothing and accept every chain a mod writes.
//
// A Herdr mod draws by describing GPUI elements (`div()`, `h_flex()`, chained
// builders). A unit test never draws anything — it drives a mod's *behaviour*,
// which is a click's effect on `hc`/`fetch`/files — so every builder here is the
// same chainable no-op, and `View` is the class a mod extends.

/**
 * Whether elements record a tree (for a test that clicks) or are the plain no-op
 * every other test uses. A recording element keeps its id, its click handlers and
 * its children, and runs `when(cond, fn)`'s `fn` when `cond` holds, so the tree is
 * what a person would see.
 */
let recording = false;
export function record(on) {
  recording = Boolean(on);
}

/** One chainable element: every method answers itself, `child`/`children` included. */
function element() {
  const node = { idv: null, click: null, down: null, kids: [] };
  const add = (c) => {
    if (Array.isArray(c)) c.forEach(add);
    else if (c !== null && c !== undefined && c !== false) node.kids.push(c);
  };
  const base = {
    id: (value) => {
      node.idv = value;
      return proxy;
    },
    child: (c) => {
      if (recording) add(c);
      return proxy;
    },
    children: (c) => {
      if (recording) add(c);
      return proxy;
    },
  };
  // Every other builder is the same no-op, so a mod's chain never names one this
  // stub lacks: a `Proxy` answers any property with the chain itself.
  const proxy = new Proxy(base, {
    get(target, name) {
      if (name === "__node") return node;
      if (name in target) return target[name];
      if (typeof name === "symbol") return undefined;
      if (recording && name === "when") return (cond, fn) => (cond ? fn(proxy) ?? proxy : proxy);
      if (recording && name === "on_click") {
        return (fn) => {
          node.click = fn;
          return proxy;
        };
      }
      return () => proxy;
    },
  });
  return proxy;
}

/** Every recorded element under `root`, root first. */
export function walk(root) {
  const out = [];
  const visit = (el) => {
    const node = el?.__node;
    if (!node) return;
    out.push(node);
    node.kids.forEach(visit);
  };
  visit(root);
  return out;
}

/**
 * Clicks the element with `id` the way the window does: its handler first, then
 * each ancestor's, until one calls `cx.stop_propagation()`. Answers what ran.
 */
export function click(root, id) {
  const path = [];
  const find = (el) => {
    const node = el?.__node;
    if (!node) return false;
    path.push(node);
    if (node.idv === id) return true;
    if (node.kids.some(find)) return true;
    path.pop();
    return false;
  };
  if (!find(root)) throw new Error(`no element ${id}`);
  const ran = [];
  let stopped = false;
  const cx = {
    stop_propagation: () => {
      stopped = true;
    },
    notify: () => {},
    timer: { after: () => ({ cancel: () => {} }) },
  };
  for (let i = path.length - 1; i >= 0 && !stopped; i -= 1) {
    const node = path[i];
    if (node.click) {
      ran.push(node.idv ?? "(unnamed)");
      node.click({}, cx);
    }
  }
  return { ran, stopped };
}

/** The base `View` a mod extends: `render` is the mod's own. */
export class View {}

/**
 * One retained text state, as the app's `InputState.new` makes it.
 *
 * A test needs the value a press of Enter would submit, and the handler the mod
 * registered, so this keeps both: `value()` is what was typed, `set_value` what
 * a send cleared, and `handlers` is how a test drives a submit without a
 * keyboard.
 */
export function inputState(options = {}) {
  let text = options?.value ?? "";
  const handlers = new Map();
  return {
    placeholder: options?.placeholder ?? "",
    handlers,
    value: () => text,
    content: () => ({ text, tokens: [] }),
    tokens: () => [],
    set_value: (next) => {
      text = typeof next === "string" ? next : (next?.text ?? "");
    },
    replace: (next) => {
      text = String(next ?? "");
    },
    on: (event, handler) => {
      handlers.set(event, handler);
      return true;
    },
    focus: () => {},
    release: () => true,
    /** What a test calls to press Enter. */
    submit: () => handlers.get("submit")?.({}, {}),
  };
}

/** The `InputState` a mod constructs. */
export const InputState = { new: (options) => inputState(options) };

/** The frame around retained text state, as the same chainable no-op. */
export const Input = { new: () => element() };

/**
 * The retained multi-line state a mod constructs: the same state an input has,
 * because a test drives a note's words and its handlers the same way.
 */
export const TextareaState = {
  new: (options) => ({
    ...inputState(options),
    set_auto_grow: () => {},
    set_rows: () => {},
  }),
};

/** The frame around a text area, the same chainable no-op as `Input`. */
export const Textarea = { new: () => element() };

/** A spinbutton frame, unused by these mods but part of the base. */
export const NumberInput = { new: () => element() };

/** Every element entry point the mods use, and any other, as the same builder. */
export const div = () => element();
export const h_flex = () => element();
export const v_flex = () => element();
export const svg = () => element();
export const image = () => element();
export const ScrollHandle = {
  new: () => ({
    /** The row indexes a mod asked to reveal, in order. */
    revealed: [],
    scroll_to_item(index) {
      this.revealed.push(index);
    },
    release: () => true,
  }),
};

/** The window's text view, drawing nothing: a test reads what a mod handed it. */
export const TextView = {
  markdown: (id, text) => {
    const node = element();
    TextView.drawn.push({ id, text });
    return node;
  },
  html: () => element(),
  /** Every markdown a mod drew, in order. */
  drawn: [],
};

export default {
  View,
  div,
  h_flex,
  v_flex,
  svg,
  image,
  ScrollHandle,
  TextView,
  InputState,
  Input,
  TextareaState,
  Textarea,
  NumberInput,
};
