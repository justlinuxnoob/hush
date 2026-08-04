/** Small shared pieces. Nothing here knows anything about Gmail. */

import { useEffect, useRef, useState } from "react";

export function Mark() {
  return (
    <span className="mark" aria-hidden="true">
      <i />
      <i />
      <i />
    </span>
  );
}

export function Wordmark() {
  return (
    <span className="wordmark">
      <Mark />
      Hush
    </span>
  );
}

export function Notice({
  tone = "calm",
  children,
}: {
  tone?: "calm" | "caution" | "problem" | "accent";
  children: React.ReactNode;
}) {
  return (
    <div
      className={`notice notice-${tone}`}
      // Problems are announced; the calm ones are just text on screen.
      role={tone === "problem" ? "alert" : undefined}
    >
      {children}
    </div>
  );
}

export function Checkbox({
  checked,
  onChange,
  label,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <span className="check">
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        aria-label={label}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span />
    </span>
  );
}

export function Meter({ value, max }: { value: number; max: number }) {
  // Without a credible total, an indeterminate bar is more honest than a
  // percentage invented from nothing.
  const indeterminate = max <= 0;
  const pct = indeterminate ? 0 : Math.min(100, Math.round((value / max) * 100));
  return (
    <div
      className={`meter${indeterminate ? " indeterminate" : ""}`}
      role="progressbar"
      aria-valuenow={indeterminate ? undefined : pct}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <i style={{ width: `${pct}%` }} />
    </div>
  );
}

export function Steps({ total, current }: { total: number; current: number }) {
  return (
    <div className="steps" role="presentation">
      {Array.from({ length: total }, (_, i) => (
        <i
          key={i}
          className={i < current ? "done" : i === current ? "now" : ""}
        />
      ))}
    </div>
  );
}

export function Switch({
  on,
  onChange,
  label,
  title,
}: {
  on: boolean;
  onChange: (v: boolean) => void;
  label: string;
  title?: string;
}) {
  return (
    <label className={`switch${on ? " on" : ""}`} title={title}>
      <input
        type="checkbox"
        checked={on}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className="track" aria-hidden="true" />
      {label}
    </label>
  );
}

/**
 * A read-only value with a copy button.
 *
 * Setup asks people to move opaque strings between two windows, and mistyping
 * one produces an error hours later. Copying should be one obvious click.
 */
export function CopyField({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      // Older webviews refuse the async clipboard; the classic path still works.
      const el = document.createElement("textarea");
      el.value = value;
      document.body.appendChild(el);
      el.select();
      document.execCommand("copy");
      el.remove();
    }
    setCopied(true);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(false), 1600);
  }

  return (
    <div>
      <label>{label}</label>
      <div className="copyrow">
        <input type="text" readOnly value={value} className="mono" />
        <button className="btn-secondary" onClick={copy}>
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
    </div>
  );
}

/** Format a timestamp as a plain date. */
export function formatDate(ms: number): string {
  if (!ms) return "—";
  return new Date(ms).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

/** "3 senders" / "1 sender" — pluralising by hand beats an Intl dependency. */
export function plural(n: number, one: string, many?: string): string {
  return `${n.toLocaleString()} ${n === 1 ? one : many ?? `${one}s`}`;
}
