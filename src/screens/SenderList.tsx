import { useEffect, useMemo, useState } from "react";

import * as api from "../api";
import { Checkbox, Notice, formatDate, plural } from "../components/ui";
import { errorMessage, methodLabel, type Sender } from "../types";

type Filter = "all" | "automatic" | "manual" | "flagged" | "handled";

const FILTERS: { value: Filter; label: string }[] = [
  { value: "all", label: "Not done yet" },
  { value: "automatic", label: "Automatic" },
  { value: "manual", label: "Needs a click" },
  { value: "flagged", label: "Worth a look" },
  { value: "handled", label: "Already done" },
];

/**
 * Senders that need nothing further.
 *
 * Deliberately narrow. A sender whose unsubscribe went through but whose old
 * mail failed to bin still has work outstanding, and hiding it because one half
 * succeeded is how you lose track of the half that did not.
 */
function isHandled(s: Sender): boolean {
  if (s.outcome === null) return false;
  if (s.outcome.status !== "done" && s.outcome.status !== "sent") return false;
  // Anything still binnable means the tidy-up did not finish its job.
  return s.bulk_count === 0;
}

/**
 * The main screen.
 *
 * Sorted busiest-first, because that is where unsubscribing pays off. Every
 * checkbox starts empty and stays that way until a person clicks it — there is
 * no "select all" that touches flagged senders, and no default selection.
 */
export default function SenderList({
  senders,
  onReload,
  onContinue,
  onRescan,
}: {
  senders: Sender[];
  onReload: () => Promise<void>;
  onContinue: (addresses: string[]) => void;
  onRescan: () => void;
}) {
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [problem, setProblem] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);

  // A rescan can remove a sender. Dropping selections that no longer exist
  // keeps the count in the action bar honest.
  useEffect(() => {
    setSelected((prev) => {
      const live = new Set(senders.map((s) => s.address));
      const next = new Set([...prev].filter((a) => live.has(a)));
      return next.size === prev.size ? prev : next;
    });
  }, [senders]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    return senders.filter((s) => {
      if (q && !`${s.display_name} ${s.address}`.toLowerCase().includes(q)) {
        return false;
      }
      // Anything already handled drops out of every view except its own. A
      // list that never shrinks as you work through it makes the work feel
      // like it did not happen.
      if (filter !== "handled" && isHandled(s)) return false;

      switch (filter) {
        case "automatic":
          return s.method.kind === "one_click";
        case "manual":
          return s.method.kind === "manual_link" || s.method.kind === "mailto";
        case "flagged":
          return s.assessment.caution;
        case "handled":
          return isHandled(s);
        default:
          return true;
      }
    });
  }, [senders, query, filter]);

  const selectable = visible.filter((s) => !s.never_touch);

  function toggle(address: string, on: boolean) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(address);
      else next.delete(address);
      return next;
    });
  }

  /** Bulk helpers never reach a flagged or protected sender. */
  function selectMany(pick: (s: Sender) => boolean) {
    setSelected((prev) => {
      const next = new Set(prev);
      for (const s of selectable) {
        if (!s.assessment.caution && pick(s)) next.add(s.address);
      }
      return next;
    });
  }

  async function protectSender(s: Sender) {
    try {
      await api.setNeverTouch(s.address, !s.never_touch);
      toggle(s.address, false);
      await onReload();
    } catch (e) {
      setProblem(errorMessage(e));
    }
  }

  const handledCount = senders.filter(isHandled).length;

  const flaggedSelected = senders.filter(
    (s) => selected.has(s.address) && s.assessment.caution
  ).length;

  return (
    <div className="list-shell">
      <div className="toolbar">
        <div className="search">
          <input
            type="search"
            placeholder="Search senders"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            aria-label="Search senders"
          />
        </div>

        <div className="row row-tight" role="group" aria-label="Filter senders">
          {FILTERS.map((f) => (
            <button
              key={f.value}
              className={filter === f.value ? "btn-secondary btn-small" : "btn-quiet btn-small"}
              aria-pressed={filter === f.value}
              onClick={() => setFilter(f.value)}
            >
              {f.label}
            </button>
          ))}
        </div>

        <div className="spacer" />
      </div>

      {problem && (
        <div style={{ padding: "calc(var(--step) * 3) calc(var(--step) * 6) 0" }}>
          <Notice tone="problem">{problem}</Notice>
        </div>
      )}

      <div className="senders">
        <div className="row" style={{ padding: "calc(var(--step) * 3) calc(var(--step) * 3)" }}>
          <span className="muted small">
            {plural(visible.length, "sender")}
            {handledCount > 0 && filter !== "handled" && (
              <> · {handledCount} already done, hidden</>
            )}
          </span>
          <div className="spacer" />
          <button
            className="btn-quiet btn-small"
            onClick={() => selectMany((s) => s.frequency.includes("month") || s.frequency.includes("year"))}
          >
            Pick the occasional ones
          </button>
          <button
            className="btn-quiet btn-small"
            onClick={() => selectMany((s) => s.method.kind === "one_click")}
          >
            Pick the automatic ones
          </button>
          <button
            className="btn-quiet btn-small"
            onClick={() => setSelected(new Set())}
            disabled={selected.size === 0}
          >
            Clear
          </button>
        </div>

        {visible.length === 0 && (
          <div className="empty stack stack-3">
            <p style={{ margin: "0 auto" }}>
              {senders.length === 0
                ? "Nobody's mailing you in bulk — or Hush hasn't looked yet."
                : handledCount === senders.length
                  ? "All done — every sender here has been handled."
                  : "Nothing matches that."}
            </p>
            {senders.length === 0 && (
              <div>
                <button className="btn-secondary" onClick={onRescan}>
                  Look through my mail
                </button>
              </div>
            )}
          </div>
        )}

        {visible.map((s) => {
          const isSelected = selected.has(s.address);
          const isOpen = expanded === s.address;
          return (
            <div
              key={s.address}
              className={[
                "sender",
                isSelected ? "is-selected" : "",
                s.assessment.caution ? "is-caution" : "",
                s.never_touch ? "is-protected" : "",
              ]
                .filter(Boolean)
                .join(" ")}
            >
              <div style={{ paddingTop: "2px" }}>
                <Checkbox
                  checked={isSelected}
                  disabled={s.never_touch}
                  onChange={(v) => toggle(s.address, v)}
                  label={`Unsubscribe from ${s.display_name}`}
                />
              </div>

              <div className="sender-main">
                <div className="sender-name">{s.display_name}</div>
                <div className="sender-address">{s.address}</div>
                <div className="sender-meta">
                  <span>{s.frequency}</span>
                  <span className="sep">·</span>
                  <span>
                    {formatDate(s.first_seen_ms)} – {formatDate(s.last_seen_ms)}
                  </span>
                  <span className="sep">·</span>
                  <span
                    className={
                      s.method.kind === "one_click" ? "badge badge-auto" : "badge badge-manual"
                    }
                  >
                    {methodLabel(s.method)}
                  </span>
                  {s.assessment.caution && (
                    <span className="badge badge-caution">
                      <span className="dot" />
                      Worth a look
                    </span>
                  )}
                  {s.never_touch && <span className="badge badge-neutral">Protected</span>}
                  {s.outcome && s.outcome.status !== "failed" && (
                    <span className="badge badge-neutral">Already handled</span>
                  )}
                </div>

                {s.assessment.caution && (
                  <div className="warn-note">
                    {s.assessment.reasons.join(". ")}. Check you don't need these
                    before unsubscribing.
                  </div>
                )}

                {isOpen && s.sample_subjects.length > 0 && (
                  <div className="panel stack stack-2" style={{ marginTop: "calc(var(--step)*2)" }}>
                    <span className="small muted">Recent subjects</span>
                    {s.sample_subjects.slice(0, 5).map((subject, i) => (
                      <span key={i} className="small">
                        {subject}
                      </span>
                    ))}
                  </div>
                )}

                <div
                  className="row row-tight sender-actions"
                  style={{ marginTop: "calc(var(--step) * 1.5)" }}
                >
                  {s.sample_subjects.length > 0 && (
                    <button
                      className="btn-quiet btn-small"
                      onClick={() => setExpanded(isOpen ? null : s.address)}
                      aria-expanded={isOpen}
                    >
                      {isOpen ? "Hide recent subjects" : "Show recent subjects"}
                    </button>
                  )}
                  <button className="btn-quiet btn-small" onClick={() => protectSender(s)}>
                    {s.never_touch ? "Stop protecting" : "Never touch this one"}
                  </button>
                </div>
              </div>

              <div className="sender-count">
                {s.message_count.toLocaleString()}
                <small>emails</small>
              </div>
            </div>
          );
        })}
      </div>

      <div className="actionbar">
        <div className="stack">
          <strong className="tabular">
            {selected.size === 0
              ? "Nothing selected"
              : `${plural(selected.size, "sender")} selected`}
          </strong>
          {flaggedSelected > 0 && (
            <span className="small" style={{ color: "var(--caution)" }}>
              {flaggedSelected} of them {flaggedSelected === 1 ? "is" : "are"} worth
              a second look
            </span>
          )}
        </div>
        <div className="spacer" />
        <button className="btn-quiet" onClick={onRescan}>
          Scan again
        </button>
        <button
          className="btn-primary"
          disabled={selected.size === 0}
          onClick={() => onContinue([...selected])}
        >
          Review and unsubscribe
        </button>
      </div>
    </div>
  );
}
