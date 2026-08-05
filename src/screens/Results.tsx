import { useState } from "react";

import * as api from "../api";
import { Notice, plural } from "../components/ui";
import { errorMessage, type Outcome, type RunReport, type Status } from "../types";

/**
 * What actually happened.
 *
 * The manual list is the part that matters: it stays put, shrinks as the user
 * ticks things off, and is the only place the app asks anything of them.
 */
export default function Results({
  status,
  report,
  onFinish,
  onDoItForReal,
}: {
  status: Status;
  report: RunReport;
  onFinish: () => void;
  /** Go straight back to the confirmation, switched to the real thing. */
  onDoItForReal: () => void;
}) {
  const [done, setDone] = useState<Set<string>>(new Set());
  const [problem, setProblem] = useState<string | null>(null);

  const simulated = report.outcomes.filter((o) => o.status === "simulated");
  const succeeded = report.outcomes.filter((o) => o.status === "done");
  const sent = report.outcomes.filter((o) => o.status === "sent");
  const needsYou = report.outcomes.filter((o) => o.status === "needs_you");
  const failed = report.outcomes.filter((o) => o.status === "failed");

  const remaining = needsYou.filter((o) => !done.has(o.address)).length;

  async function open(o: Outcome) {
    if (!o.link) return;
    try {
      await api.openLink(o.link);
    } catch (e) {
      setProblem(errorMessage(e));
    }
  }

  async function tick(o: Outcome) {
    setDone((prev) => new Set(prev).add(o.address));
    try {
      await api.markManualDone(o.address);
    } catch (e) {
      setProblem(errorMessage(e));
    }
  }

  const headline = status.dry_run
    ? "Nothing happened — that was the practice run"
    : summarise(succeeded.length + sent.length, needsYou.length);

  return (
    <div className="centre">
      <div className="inner stack stack-8">
        <div className="stack stack-3">
          <h1>{headline}</h1>
          {status.dry_run && (
            <p className="lede">
              No emails were sent, nothing was moved, and nobody was
              unsubscribed. Below is exactly what Hush would do.
            </p>
          )}
        </div>

        {problem && <Notice tone="problem">{problem}</Notice>}

        {report.trash && report.trash.trashed > 0 && (
          <Notice tone={report.trash.simulated ? "accent" : "calm"}>
            {report.trash.simulated
              ? `${plural(report.trash.trashed, "old email")} would move to your Gmail Trash.`
              : `${plural(report.trash.trashed, "old email")} moved to your Gmail Trash — recoverable there for 30 days.`}
            {report.trash.failed > 0 &&
              ` ${report.trash.failed} couldn't be moved and were left alone.`}
            {report.trash.still_present === 0 &&
              " Checked afterwards — they're gone from your inbox."}
            {report.trash.still_present !== null &&
              report.trash.still_present > 0 &&
              ` Checked afterwards, and ${report.trash.still_present} are somehow still there.`}
          </Notice>
        )}

        {simulated.length > 0 && (
          <Section title={`${plural(simulated.length, "sender")} would be handled`}>
            {simulated.map((o) => (
              <div key={o.address} className="result-row">
                <span className="result-name">{o.display_name}</span>
                <div className="spacer" />
                <span className="muted small">{withoutPrefix(o.detail)}</span>
              </div>
            ))}
          </Section>
        )}

        {succeeded.length > 0 && (
          <Section
            title={`Unsubscribe sent to ${plural(succeeded.length, "sender")}`}
            note="Their server accepted it. Most senders stop within a few days — but they don't report back, so if one keeps writing, open their page and do it by hand."
          >
            {succeeded.map((o) => (
              <div key={o.address} className="result-row">
                <span className="result-name">{o.display_name}</span>
                <div className="spacer" />
                <span className="badge badge-auto">Accepted</span>
                {o.link && (
                  <button className="btn-quiet btn-small" onClick={() => open(o)}>
                    Open their page
                  </button>
                )}
              </div>
            ))}
          </Section>
        )}

        {sent.length > 0 && (
          <Section
            title={`${plural(sent.length, "unsubscribe email")} sent`}
            note="Most senders act on these within a few days."
          >
            {sent.map((o) => (
              <div key={o.address} className="result-row">
                <span className="result-name">{o.display_name}</span>
                <div className="spacer" />
                <span className="badge badge-neutral">Sent</span>
              </div>
            ))}
          </Section>
        )}

        {needsYou.length > 0 && (
          <Section
            // Deliberately not a restatement of the headline, which already
            // carries the count — two identical lines read as a rendering fault.
            title={
              remaining === 0
                ? "All finished — nice work"
                : `Finish these ${remaining} yourself`
            }
            note="These senders only offer a link, and a link can mean anything, so Hush leaves it to you."
          >
            {needsYou.map((o) => {
              const isDone = done.has(o.address);
              return (
                <div key={o.address} className={`result-row${isDone ? " done" : ""}`}>
                  <span className="result-name">{o.display_name}</span>
                  <div className="spacer" />
                  {!isDone && o.link && (
                    <button className="btn-secondary btn-small" onClick={() => open(o)}>
                      Open link
                    </button>
                  )}
                  {!isDone && !o.link && (
                    <span className="muted small">Check your mail app</span>
                  )}
                  <button
                    className={isDone ? "btn-quiet btn-small" : "btn-quiet btn-small"}
                    onClick={() => tick(o)}
                    disabled={isDone}
                  >
                    {isDone ? "Done" : "Mark as done"}
                  </button>
                </div>
              );
            })}
          </Section>
        )}

        {failed.length > 0 && (
          <Section
            title={`${plural(failed.length, "sender")} didn't work`}
            note="Nothing was changed for these. You can try again, or open the link yourself."
          >
            {failed.map((o) => (
              <div key={o.address} className="result-row">
                <div className="stack" style={{ minWidth: 0 }}>
                  <span className="result-name">{o.display_name}</span>
                  <span className="muted small">{o.detail}</span>
                </div>
                <div className="spacer" />
                {o.link && (
                  <button className="btn-secondary btn-small" onClick={() => open(o)}>
                    Open link
                  </button>
                )}
              </div>
            ))}
          </Section>
        )}

        {status.dry_run ? (
          <div className="card stack stack-3">
            <div className="stack">
              <strong>Happy with that? Do it for real.</strong>
              <span className="muted small">
                Same senders, same choices — except this time it actually
                happens.
              </span>
            </div>
            <div className="row">
              <button className="btn-primary" onClick={onDoItForReal} autoFocus>
                Do it for real
              </button>
              <button className="btn-quiet" onClick={onFinish}>
                Back to the list
              </button>
            </div>
          </div>
        ) : (
          <div className="row">
            <button className="btn-primary" onClick={onFinish} autoFocus>
              Back to the list
            </button>
            <span className="muted small">
              Senders usually stop within a few days. Nothing was deleted.
            </span>
          </div>
        )}
      </div>
    </div>
  );
}

function Section({
  title,
  note,
  children,
}: {
  title: string;
  note?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="result-group stack stack-3">
      <h2>{title}</h2>
      {note && <p className="muted small">{note}</p>}
      <div>{children}</div>
    </div>
  );
}

/**
 * Drop the "Dry run — " lead-in, which the heading already says, and restore
 * the capital the strip takes with it.
 */
function withoutPrefix(detail: string): string {
  const rest = detail.replace(/^Dry run\s*[—-]\s*/, "");
  return rest.charAt(0).toUpperCase() + rest.slice(1);
}

/** "42 unsubscribed, 8 need one click from you." */
function summarise(automatic: number, manual: number): string {
  if (automatic === 0 && manual === 0) return "Nothing to report";
  if (manual === 0) return `Unsubscribe sent to ${plural(automatic, "sender")}`;
  if (automatic === 0)
    return `${plural(manual, "sender")} need one click from you`;
  return `${automatic} unsubscribed, ${manual} need one click from you`;
}
