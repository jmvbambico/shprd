// The status glyph beside a task's word: Shepherdr's shapes, 14 px, in the
// status's tone. A working task's pie takes the progress it reports, so a state
// change never moves the row.

import type { Glyph as Shape, Tone } from "./board";

/** What the glyph needs. */
export interface GlyphProps {
  /** The shape. */
  shape: Shape;
  /** The colour word. */
  tone: Tone;
  /** How far a working task said it had got, 0–100. */
  progress?: number;
  /** The accessible name; empty hides it (the row already says the word). */
  label?: string;
}

/** The glyph, as an inline SVG. */
export function Glyph({ shape, tone, progress, label = "" }: GlyphProps) {
  const c = 7;
  const r = 5.5;
  let body;
  switch (shape) {
    case "ring":
      body = <circle cx={c} cy={c} r={r} fill="none" stroke="currentColor" strokeWidth="1.5" />;
      break;
    case "calendar":
      body = (
        <>
          <rect x="2" y="3" width="10" height="9" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
          <path d="M2 6h10M5 1.5v3M9 1.5v3" stroke="currentColor" strokeWidth="1.3" fill="none" />
        </>
      );
      break;
    case "clock":
      body = (
        <>
          <circle cx={c} cy={c} r={r} fill="none" stroke="currentColor" strokeWidth="1.5" />
          <path d="M7 4v3.2l2 1.3" stroke="currentColor" strokeWidth="1.3" fill="none" strokeLinecap="round" />
        </>
      );
      break;
    case "pie": {
      const fraction = Math.min(1, Math.max(0.08, (progress ?? 0) / 100));
      const angle = fraction * Math.PI * 2 - Math.PI / 2;
      const x = c + r * Math.cos(angle);
      const y = c + r * Math.sin(angle);
      body = (
        <>
          <circle cx={c} cy={c} r={r} fill="none" stroke="currentColor" strokeWidth="1.5" opacity="0.45" />
          <path
            d={`M${c} ${c} L${c} ${c - r} A${r} ${r} 0 ${fraction > 0.5 ? 1 : 0} 1 ${x.toFixed(2)} ${y.toFixed(2)} Z`}
            fill="currentColor"
          />
        </>
      );
      break;
    }
    case "dot":
      body = <circle cx={c} cy={c} r="4" fill="currentColor" />;
      break;
    case "tick":
      body = (
        <>
          <circle cx={c} cy={c} r={r} fill="none" stroke="currentColor" strokeWidth="1.5" />
          <path d="M4.5 7.2l1.7 1.7 3.3-3.6" stroke="currentColor" strokeWidth="1.5" fill="none" strokeLinecap="round" strokeLinejoin="round" />
        </>
      );
      break;
    case "spin":
      body = (
        <circle
          className="glyph-spin-arc"
          cx={c}
          cy={c}
          r={r}
          fill="none"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeDasharray="14 22"
          strokeLinecap="round"
        />
      );
      break;
  }
  return (
    <svg
      className="glyph"
      data-tone={tone}
      data-shape={shape}
      width="14"
      height="14"
      viewBox="0 0 14 14"
      role={label === "" ? undefined : "img"}
      aria-label={label === "" ? undefined : label}
      aria-hidden={label === "" ? true : undefined}
    >
      {body}
    </svg>
  );
}
