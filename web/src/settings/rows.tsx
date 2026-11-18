// The row anatomy every page shares: a title and one line about it on the left,
// the value and the layer it came from at the right, a control beside them, and
// `below` — a list of choices or a box — under all of it.
//
// Source: eliasstravik/shepherdr `mods/shepherdr-settings/main.js` (`row()`,
// `scopeLine()`), commit a3f327a, MIT (see the header of `model.ts`).

import { type ReactNode } from "react";

import { CHANGED_HERE, RESET, type ScopeId } from "./model";

/** What a row says about where its value came from. */
export interface RowScope {
  /** The settings key, for the Reset link's label. */
  key: string;
  /** The layer's words. */
  layer: string;
  /** Whether this repository's entry set the value. */
  mine: boolean;
}

/** The props of one row. */
export interface RowProps {
  /** The title. */
  title: string;
  /** The line under it. */
  about?: string;
  /** The value shown at the right. */
  value?: string;
  /** The layer line. */
  scope?: RowScope;
  /** Which scope the page writes to. */
  scopeId: ScopeId;
  /** Puts the row back to what it inherits (repo scope only). */
  onReset?: () => void;
  /** Disables Reset while a write is in flight. */
  busy?: boolean;
  /** The control beside the value. */
  control?: ReactNode;
  /** What shows under the row. */
  below?: ReactNode;
  /** A test hook and an anchor for the row. */
  id?: string;
}

/** One row. */
export function Row({
  title,
  about,
  value,
  scope,
  scopeId,
  onReset,
  busy = false,
  control,
  below,
  id,
}: RowProps) {
  return (
    <div className="srow" data-row={id}>
      <div className="srow-main">
        <div className="srow-words">
          <div className="srow-title">{title}</div>
          {about ? <div className="srow-about">{about}</div> : null}
        </div>
        {value !== undefined || scope ? (
          <div className="srow-value">
            {value !== undefined ? <span className="srow-display">{value}</span> : null}
            {scope ? (
              <ScopeLine scope={scope} scopeId={scopeId} onReset={onReset} busy={busy} />
            ) : null}
          </div>
        ) : null}
        {control ? <div className="srow-control">{control}</div> : null}
      </div>
      {below}
    </div>
  );
}

/**
 * Where a row's value comes from.
 *
 * In `This repo` the row says whether the repository set its own (`Set for this
 * repository · Reset`) or which layer it inherits from; in `You` it names the
 * layer that answered, in the service's own words.
 */
export function ScopeLine({
  scope,
  scopeId,
  onReset,
  busy,
}: {
  scope: RowScope;
  scopeId: ScopeId;
  onReset?: (() => void) | undefined;
  busy: boolean;
}) {
  if (scopeId !== "repo") {
    return <span className="srow-layer">{scope.layer}</span>;
  }
  if (scope.mine) {
    return (
      <span className="srow-layer">
        <span className="srow-mine">{CHANGED_HERE}</span> ·{" "}
        <button
          type="button"
          className="link"
          aria-label={`${RESET} ${scope.key}`}
          disabled={busy}
          onClick={onReset}
        >
          {RESET}
        </button>
      </span>
    );
  }
  return <span className="srow-layer">inherited from {scope.layer}</span>;
}

/** A segmented control: one button per choice, the current one marked. */
export function Segmented({
  label,
  options,
  value,
  onPick,
  disabled = false,
}: {
  label: string;
  options: Array<[string, string]>;
  value: string | null;
  onPick: (value: string) => void;
  disabled?: boolean;
}) {
  return (
    <div className="segmented" role="group" aria-label={label}>
      {options.map(([id, word]) => (
        <button
          key={id}
          type="button"
          className="seg"
          aria-pressed={value === id}
          disabled={disabled}
          onClick={() => onPick(id)}
        >
          {word}
        </button>
      ))}
    </div>
  );
}

/** A switch: one button that says whether it is on. */
export function Switch({
  label,
  on,
  onToggle,
  disabled = false,
}: {
  label: string;
  on: boolean;
  onToggle: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      className="switch"
      disabled={disabled}
      onClick={onToggle}
    >
      <span className="switch-knob" aria-hidden="true" />
    </button>
  );
}
