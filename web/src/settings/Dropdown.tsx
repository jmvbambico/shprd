// The popovers every dropdown of the Settings page opens: a plain pick list, and
// the Agents page's model menu.
//
// Shepherdr's pickers are popovers over the page (`mods/chat-models`), not lists
// drawn into the row, so a click on the button opens something that is above
// everything and closes on a click beside it or on Esc. These are the same two
// shapes for the web, sharing the rules in `menu.ts` with the Herdr window.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat-models/main.js`, `mods/shepherdr-settings/settings.js`
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import { useEffect, useRef, useState } from "react";

import {
  chooseModel,
  fastOffered,
  levelOf,
  levelWord,
  levelsFor,
  modelRows,
  SELECT_HARNESS,
  type Menus,
  type Selection,
} from "./menu";

/** Open/closed state for a popover: closes on a press outside it and on Esc (before the dialog sees Esc). */
export function usePopover() {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) {
      return undefined;
    }
    function onDown(event: MouseEvent) {
      if (root.current && !root.current.contains(event.target as Node)) {
        setOpen(false);
      }
    }
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.stopPropagation();
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [open]);
  return { open, setOpen, root };
}

/** One option of a pick list. */
export interface DropdownOption {
  id: string;
  label: string;
  hint?: string;
}

/** A button with a chevron that opens a pick list; the current option is checked. */
export function Dropdown({
  label,
  value,
  options,
  onPick,
  bordered = true,
  disabled = false,
  shown,
}: {
  /** What the button is called (an aria-label). */
  label: string;
  value: string | null;
  options: DropdownOption[];
  onPick: (id: string) => void;
  bordered?: boolean;
  disabled?: boolean;
  /** The words on the button; the current option's label when absent. */
  shown?: string;
}) {
  const { open, setOpen, root } = usePopover();
  const current = options.find((option) => option.id === value);
  return (
    <div className="pop" ref={root}>
      <button
        type="button"
        className={bordered ? "pop-button bordered" : "pop-button"}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={label}
        disabled={disabled}
        onClick={() => setOpen(!open)}
      >
        <span>{shown ?? current?.label ?? "Choose"}</span>
        <span className="pop-chevron" aria-hidden="true">
          ⌄
        </span>
      </button>
      {open ? (
        <div className="pop-panel" role="listbox" aria-label={label}>
          {options.map((option) => (
            <button
              key={option.id}
              type="button"
              role="option"
              aria-selected={option.id === value}
              className="pop-row"
              onClick={() => {
                setOpen(false);
                onPick(option.id);
              }}
            >
              <span className="pop-row-label">{option.label}</span>
              {option.hint ? <span className="pop-row-hint">{option.hint}</span> : null}
              <span className="pop-check" aria-hidden="true">
                {option.id === value ? "✓" : ""}
              </span>
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

/**
 * The model menu: a program dropdown (every detected program: a row of tabs does
 * not scale to many CLIs), a search over every program's models, the models with a
 * check on the current one, Reasoning, Fast and Permissions. Every change goes back
 * at once through `onChange`.
 *
 * `light` is a program-and-model choice only (the reviewer's fallback): no
 * Reasoning, Fast or Permissions, and the menu closes on a pick. `onNone` adds a
 * "None" row. `taken` lists programs another line of the same role runs: shown,
 * not pickable.
 */
export function ModelMenu({
  label,
  shown,
  menus,
  sel,
  onChange,
  disabled = false,
  light = false,
  onNone,
  taken = [],
}: {
  label: string;
  shown: string;
  menus: Menus;
  sel: Selection;
  onChange: (next: Selection) => void;
  disabled?: boolean;
  light?: boolean;
  onNone?: () => void;
  taken?: string[];
}) {
  const { open, setOpen, root } = usePopover();
  const [tab, setTab] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [permOpen, setPermOpen] = useState(false);
  const [progOpen, setProgOpen] = useState(false);
  const active = tab ?? (menus.programs[sel.agent ?? ""] ? sel.agent! : (menus.order[0] ?? null));
  const rows = active ? modelRows(menus, active, query) : [];
  const levels = levelsFor(menus, sel);
  const level = levelOf(menus, sel);
  const program = menus.programs[sel.agent ?? ""] ?? null;
  const perm =
    program?.perms.find((entry) => entry.id === sel.perm) ??
    program?.perms.find((entry) => entry.id === program.defaultPerm) ??
    null;

  function openMenu() {
    setTab(null);
    setQuery("");
    setPermOpen(false);
    setProgOpen(false);
    setOpen(!open);
  }

  function pickModel(row: { agent: string; id: string; typed: boolean }) {
    const unlisted = row.typed
      ? { id: row.id, name: row.id, efforts: [], defaultEffort: null, fast: false }
      : undefined;
    setTab(row.agent);
    setQuery("");
    onChange(chooseModel(menus, sel, row.agent, row.id, unlisted));
    if (light) {
      setOpen(false);
    }
  }

  return (
    <div className="pop" ref={root}>
      <button
        type="button"
        className="pop-button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={label}
        disabled={disabled}
        onClick={openMenu}
      >
        <span>{shown}</span>
        <span className="pop-chevron" aria-hidden="true">
          ⌄
        </span>
      </button>
      {open ? (
        <div className="pop-panel model-menu" role="dialog" aria-label={`${label}: model menu`}>
          {menus.order.length > 0 ? (
            <button
              type="button"
              className="mm-program"
              aria-haspopup="listbox"
              aria-expanded={progOpen}
              aria-label="Program"
              onClick={() => setProgOpen(!progOpen)}
            >
              <span className="mm-program-name">
                {active ? menus.programs[active]!.name : SELECT_HARNESS}
              </span>
              <span className="pop-chevron" aria-hidden="true">
                ⌄
              </span>
            </button>
          ) : null}
          <input
            className="mm-search"
            aria-label="Search every agent’s models"
            placeholder="Search every agent’s models"
            autoFocus
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
          {onNone && !progOpen ? (
            <button
              type="button"
              role="option"
              aria-selected={!sel.agent}
              className="pop-row"
              onClick={() => {
                onNone();
                setOpen(false);
              }}
            >
              <span className="pop-row-label">None</span>
              <span className="pop-check" aria-hidden="true">
                {!sel.agent ? "✓" : ""}
              </span>
            </button>
          ) : null}
          {progOpen ? (
            <>
              <div className="mm-head">Programs</div>
              <div className="mm-list" role="listbox" aria-label="Programs">
                {menus.order.map((id) => {
                  const blocked = taken.includes(id);
                  return (
                    <button
                      key={id}
                      type="button"
                      role="option"
                      aria-selected={id === active}
                      aria-disabled={blocked}
                      disabled={blocked}
                      className="pop-row"
                      onClick={() => {
                        setTab(id);
                        setQuery("");
                        setProgOpen(false);
                      }}
                    >
                      <span className="pop-row-label">{menus.programs[id]!.name}</span>
                      {blocked ? <span className="pop-row-hint">another line has it</span> : null}
                      <span className="pop-check" aria-hidden="true">
                        {id === active ? "✓" : ""}
                      </span>
                    </button>
                  );
                })}
              </div>
            </>
          ) : (
            <>
          <div className="mm-head">Models</div>
          <div className="mm-list" role="listbox" aria-label="Models">
            {rows.length === 0 ? <div className="mm-note">No models.</div> : null}
            {rows.map((row) => (
              <button
                key={`${row.agent}:${row.id}`}
                type="button"
                role="option"
                aria-selected={!row.typed && row.agent === sel.agent && row.id === sel.model}
                className="pop-row"
                onClick={() => pickModel(row)}
              >
                <span className="pop-row-label">{row.name}</span>
                {row.typed ? (
                  <span className="pop-row-hint">{menus.programs[row.agent]?.name} id</span>
                ) : null}
                <span className="pop-check" aria-hidden="true">
                  {!row.typed && row.agent === sel.agent && row.id === sel.model ? "✓" : ""}
                </span>
              </button>
            ))}
          </div>
            </>
          )}
          {!progOpen && active && menus.programs[active]?.note ? (
            <div className="mm-note">{menus.programs[active]!.note}</div>
          ) : null}
          {!light && levels.length > 0 ? (
            <>
              <div className="mm-head">Reasoning</div>
              <div className="mm-levels" role="radiogroup" aria-label="Reasoning">
                {levels.map((entry) => (
                  <button
                    key={entry}
                    type="button"
                    role="radio"
                    aria-checked={entry === level}
                    className="mm-level"
                    onClick={() => onChange({ ...sel, effort: entry })}
                  >
                    {levelWord(entry)}
                  </button>
                ))}
              </div>
            </>
          ) : null}
          {!light && fastOffered(menus, sel) ? (
            <button
              type="button"
              role="switch"
              aria-checked={sel.fast}
              className="pop-row"
              onClick={() => onChange({ ...sel, fast: !sel.fast })}
            >
              <span className="pop-row-label">Fast</span>
              <span className={sel.fast ? "switch on" : "switch"} aria-hidden="true">
                <span className="switch-knob" />
              </span>
            </button>
          ) : null}
          {!light && program && program.perms.length > 0 ? (
            <div className="mm-perms">
              <button
                type="button"
                className="pop-row"
                aria-haspopup="menu"
                aria-expanded={permOpen}
                onClick={() => setPermOpen(!permOpen)}
              >
                <span className="pop-row-label">Permissions</span>
                <span className="pop-row-hint">{perm?.label ?? ""}</span>
                <span className="pop-chevron-right" aria-hidden="true">
                  ›
                </span>
              </button>
              {permOpen ? (
                <div className="pop-panel mm-permmenu" role="menu" aria-label="Permissions">
                  {program.perms.map((entry) => (
                    <button
                      key={entry.id}
                      type="button"
                      role="menuitemradio"
                      aria-checked={entry.id === perm?.id}
                      className="pop-row"
                      onClick={() => {
                        setPermOpen(false);
                        onChange({ ...sel, perm: entry.id });
                      }}
                    >
                      <span className="pop-row-label">{entry.label}</span>
                      <span className="pop-check" aria-hidden="true">
                        {entry.id === perm?.id ? "✓" : ""}
                      </span>
                    </button>
                  ))}
                </div>
              ) : null}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
