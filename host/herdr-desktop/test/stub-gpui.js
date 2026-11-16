// The `gpui-kit` and `gpui-base` modules, stubbed for a unit test: element
// builders that record nothing and accept every chain a mod writes.
//
// A Herdr mod draws by describing GPUI elements (`div()`, `h_flex()`, chained
// builders). A unit test never draws anything — it drives a mod's *behaviour*,
// which is a click's effect on `hc`/`fetch`/files — so every builder here is the
// same chainable no-op, and `View` is the class a mod extends.

/** One chainable element: every method answers itself, `child`/`children` included. */
function element() {
  const node = {
    id: () => node,
    child: () => node,
    children: () => node,
  };
  // Every other builder is the same no-op, so a mod's chain never names one this
  // stub lacks: a `Proxy` answers any property with the chain itself.
  return new Proxy(node, {
    get(target, name) {
      if (name in target) return target[name];
      if (typeof name === "symbol") return undefined;
      return () => node;
    },
  });
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

/** Every element entry point the mods use, and any other, as the same builder. */
export const div = () => element();
export const h_flex = () => element();
export const v_flex = () => element();
export const svg = () => element();
export const image = () => element();
export const ScrollHandle = { new: () => ({}) };

export default {
  View,
  div,
  h_flex,
  v_flex,
  svg,
  image,
  ScrollHandle,
  InputState,
  Input,
};
