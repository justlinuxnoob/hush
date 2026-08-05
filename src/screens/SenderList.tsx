import { memo, useCallback, useEffect, useMemo, useState } from "react";

import * as api from "../api";
import { Checkbox, Notice, formatCount, formatDate, plural } from "../components/ui";
import {
  errorMessage,
  methodLabel,
  type Sender,
  type SenderMessage,
} from "../types";

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
 * This used to also require `bulk_count === 0`, on the reasoning that a sender
 * whose unsubscribe worked but whose backlog failed to bin still has work
 * outstanding. That conflated *failed to bin* with *was never asked to bin* —
 * and binning is opt-in, off by default. So the ordinary path, unsubscribing
 * and nothing else, left every sender looking unfinished: still in the main
 * list, never appearing under "Already done", no sign anything had happened.
 *
 * The unsubscribe is the job. Leftover mail is visible in the count on the row,
 * and a run that fails to bin says so on the results screen.
 */
function isHandled(s: Sender): boolean {
  if (s.outcome === null) return false;
  return s.outcome.status === "done" || s.outcome.status === "sent";
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
  // Fetched when a row is opened rather than bundled into every sender: the
  // list holds a handful of samples, this holds the lot.
  const [subjects, setSubjects] = useState<SenderMessage[] | null>(null);

  // The three row callbacks are stable, so `Row`'s memo comparison holds. A
  // fresh closure each render would make every row's props "change" and undo
  // the memo entirely.
  const openSubjects = useCallback(async (address: string) => {
    let opening = true;
    setExpanded((prev) => {
      opening = prev !== address;
      return opening ? address : null;
    });
    if (!opening) return;
    setSubjects(null);
    try {
      setSubjects(await api.senderMessages(address));
    } catch (e) {
      setProblem(errorMessage(e));
    }
  }, []);

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

  const toggle = useCallback((address: string, on: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(address);
      else next.delete(address);
      return next;
    });
  }, []);

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

  const protectSender = useCallback(
    async (s: Sender) => {
      try {
        await api.setNeverTouch(s.address, !s.never_touch);
        toggle(s.address, false);
        await onReload();
      } catch (e) {
        setProblem(errorMessage(e));
      }
    },
    [toggle, onReload]
  );

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

        {visible.map((s) => (
          <Row
            key={s.address}
            sender={s}
            selected={selected.has(s.address)}
            open={expanded === s.address}
            subjects={expanded === s.address ? subjects : null}
            onToggle={toggle}
            onOpen={openSubjects}
            onProtect={protectSender}
          />
        ))}
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

/**
 * One sender, memoised.
 *
 * Ticking a checkbox changes one row, but React re-renders whatever it is told
 * to. Without this the whole list re-rendered on every click — at 157 senders
 * that measured 559ms per tick, which is what "the app feels laggy" was. The
 * props are deliberately primitives plus stable callbacks so the comparison
 * actually short-circuits; passing `selected` as the Set would defeat it,
 * because a new Set is a new object every time.
 */
const Row = memo(function Row({
  sender: s,
  selected,
  open,
  subjects,
  onToggle,
  onOpen,
  onProtect,
}: {
  sender: Sender;
  selected: boolean;
  open: boolean;
  subjects: SenderMessage[] | null;
  onToggle: (address: string, on: boolean) => void;
  onOpen: (address: string) => void;
  onProtect: (s: Sender) => void;
}) {
      const isSelected = selected;
      const isOpen = open;
      return (
        <div
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
              onChange={(v) => onToggle(s.address, v)}
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

            {isOpen && (
              <div className="panel stack stack-2" style={{ marginTop: "calc(var(--step)*2)" }}>
                <span className="small muted">
                  {subjects === null
                    ? "Loading…"
                    : `Everything from them that Hush has seen (${subjects.length})`}
                </span>
                {subjects !== null && (
                  <div className="subject-list stack stack-1">
                    {subjects.map((m, i) => (
                      <div key={i} className="subject-row">
                        <span className="small">{m.subject}</span>
                        <span className="muted small tabular">
                          {formatDate(m.date_ms)}
                        </span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}

            <div
              className="row row-tight sender-actions"
              style={{ marginTop: "calc(var(--step) * 1.5)" }}
            >
              {s.message_count > 0 && (
                <button
                  className="btn-quiet btn-small"
                  onClick={() => onOpen(s.address)}
                  aria-expanded={isOpen}
                >
                  {isOpen ? "Hide their emails" : `Show all ${s.message_count} emails`}
                </button>
              )}
              <button className="btn-quiet btn-small" onClick={() => onProtect(s)}>
                {s.never_touch ? "Stop protecting" : "Never touch this one"}
              </button>
            </div>
          </div>

          <div className="sender-count">
            {formatCount(s.message_count)}
            <small>emails</small>
          </div>
        </div>
      );
});
