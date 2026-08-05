import { useState } from "react";

import * as api from "../api";
import { Notice, plural } from "../components/ui";
import { errorMessage, type Outcome, type RunReport } from "../types";

/**
 * What actually happened.
 *
 * The manual list is the part that matters: it stays put, shrinks as the user
 * ticks things off, and is the only place the app asks anything of them.
 */
export default function Results({
  report,
  onFinish,
}: {
  report: RunReport;
  onFinish: () => void;
}) {
  const [done, setDone] = useState<Set<string>>(new Set());
  const [problem, setProblem] = useState<string | null>(null);

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

  const acted = succeeded.length + sent.length + needsYou.length + failed.length;
  const binned = report.trash?.trashed ?? 0;

  // A bin-only run has no unsubscribe outcomes at all, and "Nothing to report"
  // would be a strange thing to say after moving five hundred emails.
  const headline =
    acted === 0 && binned > 0
      ? `${plural(binned, "old email")} moved to Trash`
      : summarise(succeeded.length + sent.length, needsYou.length);

  return (
    <div className="centre">
      <div className="inner stack stack-8">
        <div className="stack stack-3">
          <h1>{headline}</h1>
        </div>

        {problem && <Notice tone="problem">{problem}</Notice>}

        {!report.trash && (
          <Notice tone="calm">
            Old emails were left alone — you didn't ask for those to be binned.
            Unsubscribing stops what arrives next; it never removes what's
            already there.
          </Notice>
        )}

        {report.trash && report.trash.trashed === 0 && report.trash.failed > 0 && (
          <Notice tone="problem">
            Nothing could be moved to Trash. {report.trash.problem ?? "Google refused the request."}{" "}
            {report.trash.failed} {report.trash.failed === 1 ? "email was" : "emails were"} left
            exactly where they are.
          </Notice>
        )}

        {report.trash && report.trash.trashed === 0 && report.trash.failed === 0 && (
          <Notice tone="caution">
            There was nothing to move. Hush can only bin emails it has scanned,
            so this means either a previous run already cleared them, or the
            scan never reached this sender's mail — try scanning again, and let
            it finish.
          </Notice>
        )}

        {report.blocked && report.blocked.blocked === 0 && report.blocked.failed > 0 && (
          <Notice tone="problem">
            Nothing was blocked.{" "}
            {report.blocked.problem ?? "Google refused to create the filter."}{" "}
            {report.blocked.problem?.includes("permission")
              ? "Go back and press \u201cAllow Hush to block senders\u201d — Google will ask you once, and blocking works from then on."
              : "These senders can still reach your inbox."}
          </Notice>
        )}

        {report.blocked && report.blocked.blocked > 0 && (
          <Notice tone="accent">
            {plural(report.blocked.blocked, "sender")} blocked — anything they
            send from now on goes straight to Trash, whether or not they honour
            the unsubscribe. You can see and undo these under Settings → Filters
            in Gmail.
            {report.blocked.confirmed !== null &&
              report.blocked.confirmed === report.blocked.blocked &&
              " Checked afterwards — the filters are there."}
            {" "}
            Gmail's own "Block sender" button stays on every email whether you've
            filtered them or not, so it isn't a way to tell — the Filters tab is.
            {report.blocked.confirmed !== null &&
              report.blocked.confirmed < report.blocked.blocked &&
              ` Checked afterwards, and only ${report.blocked.confirmed} of them actually exist.`}
            {report.blocked.failed > 0 &&
              ` ${report.blocked.failed} couldn't be blocked${
                report.blocked.problem ? ` — ${report.blocked.problem}` : ""
              }.`}
          </Notice>
        )}

        {report.trash && report.trash.trashed > 0 && (
          <Notice tone="calm">
            {`${plural(report.trash.trashed, "old email")} moved to your Gmail Trash — recoverable there for 30 days.`}
            {report.trash.failed > 0 &&
              ` ${report.trash.failed} couldn't be moved${
                report.trash.problem ? ` — ${report.trash.problem}` : ""
              }.`}
            {report.trash.still_present === 0 &&
              " Checked afterwards — they're gone from your inbox."}
            {report.trash.still_present !== null &&
              report.trash.still_present > 0 &&
              ` Checked afterwards, and ${report.trash.still_present} are somehow still there.`}
          </Notice>
        )}


        {succeeded.length > 0 && (
          <Section title={`${plural(succeeded.length, "sender")} finished`}>
            {succeeded.map((o) => (
              <div key={o.address} className="result-row">
                <span className="result-name">{o.display_name}</span>
                <div className="spacer" />
                <span className="badge badge-auto">Done</span>
              </div>
            ))}
          </Section>
        )}

        {sent.length > 0 && (
          <Section
            title={`Unsubscribe sent to ${plural(sent.length, "sender")}`}
            note="Their server accepted it, which is as far as anything can be confirmed — nothing in email reports back that a sender actually acted. Most stop within a few days. A few accept the request and still want you to press a button on their own page; if you want to be certain, check."
          >
            {sent.map((o) => (
              <div key={o.address} className="result-row">
                <div className="stack" style={{ minWidth: 0 }}>
                  <span className="result-name">{o.display_name}</span>
                  <span className="muted small">{o.detail}</span>
                </div>
                <div className="spacer" />
                {o.link && (
                  <button className="btn-secondary btn-small" onClick={() => open(o)}>
                    Check it yourself
                  </button>
                )}
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
            note="Some of these only offer a link, which can mean anything, so Hush won't click it blindly. Others need a short email — if no draft opened, your computer has no mail app set up, and you can just send it yourself from the address shown."
          >
            {needsYou.map((o) => {
              const isDone = done.has(o.address);
              const byEmail = o.link?.startsWith("mailto:") ?? false;
              return (
                <div key={o.address} className={`result-row${isDone ? " done" : ""}`}>
                  <div className="stack" style={{ minWidth: 0 }}>
                    <span className="result-name">{o.display_name}</span>
                    <span className="muted small">{o.detail}</span>
                  </div>
                  <div className="spacer" />
                  {!isDone && o.link && (
                    <button className="btn-secondary btn-small" onClick={() => open(o)}>
                      {byEmail ? "Open the draft" : "Open link"}
                    </button>
                  )}
                  {!isDone && !o.link && (
                    <span className="muted small">Nothing to open</span>
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

          <div className="row">
            <button className="btn-primary" onClick={onFinish} autoFocus>
              Back to the list
            </button>
            <span className="muted small">
              Senders usually stop within a few days. Nothing was deleted.
            </span>
          </div>
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

/** "42 unsubscribed, 8 need one click from you." */
function summarise(automatic: number, manual: number): string {
  if (automatic === 0 && manual === 0) return "Nothing to report";
  if (manual === 0) return `Unsubscribe sent to ${plural(automatic, "sender")}`;
  if (automatic === 0)
    return `${plural(manual, "sender")} need one click from you`;
  return `${automatic} unsubscribed, ${manual} need one click from you`;
}
