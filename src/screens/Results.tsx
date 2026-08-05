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
  const [problem, setProblem] = useState<string | null>(null);

  const succeeded = report.outcomes.filter((o) => o.status === "done");
  const sent = report.outcomes.filter((o) => o.status === "sent");
  const couldNotAutomate = report.outcomes.filter((o) => o.status === "could_not_automate");
  const failed = report.outcomes.filter((o) => o.status === "failed");


  async function open(o: Outcome) {
    if (!o.link) return;
    try {
      await api.openLink(o.link);
    } catch (e) {
      setProblem(errorMessage(e));
    }
  }

  // Whether blocking covered the senders nothing automatic could reach.
  const blockedOk = (report.blocked?.blocked ?? 0) > 0;
  const binned = report.trash?.trashed ?? 0;

  // A bin-only run has no unsubscribe outcomes at all, and "Nothing to report"
  // would be a strange thing to say after moving five hundred emails.
  const headline = summarise({
    unsubscribed: succeeded.length + sent.length,
    blocked: report.blocked?.blocked ?? 0,
    binned,
    leftForYou: blockedOk ? 0 : couldNotAutomate.length,
  });

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
            {plural(report.blocked.blocked, "sender")} blocked —{" "}
            {report.blocked.action === "trash"
              ? "anything they send from now on goes straight to Trash, whether or not they honour the unsubscribe. Gmail empties Trash after 30 days."
              : "anything they send from now on skips your inbox, whether or not they honour the unsubscribe. It stays in your account and stays searchable; nothing is deleted."}{" "}
            Undo any of them under Blocked senders.
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
            {report.blocked.unmarked &&
              " Hush couldn't add its own label to these, so it won't be able to list or undo them itself — you'd remove them under Settings → Filters in Gmail."}
          </Notice>
        )}

        {report.trash && report.trash.trashed > 0 && (
          <Notice tone="calm">
            {report.trash.action === "trash"
              ? `${plural(report.trash.trashed, "old email")} moved to your Gmail Trash — recoverable there for 30 days.`
              : `${plural(report.trash.trashed, "old email")} cleared out of your inbox. They're still in your account, under the Hush label — nothing was deleted.`}
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

        {couldNotAutomate.length > 0 && (
          <Section
            title={`${plural(couldNotAutomate.length, "sender")} couldn't be unsubscribed automatically`}
            note={
              blockedOk
                ? `They only offer an unsubscribe you'd have to click through yourself, so Hush blocked them instead — their mail ${
                    report.blocked?.action === "trash"
                      ? "goes to Trash"
                      : "skips your inbox"
                  } from now on and there's nothing for you to do.`
                : "They only offer an unsubscribe you'd have to click through yourself. Blocking would handle these without any work from you — it's the option on the previous screen."
            }
          >
            {couldNotAutomate.map((o) => (
              <div key={o.address} className="result-row">
                <div className="stack" style={{ minWidth: 0 }}>
                  <span className="result-name">{o.display_name}</span>
                  <span className="muted small">{o.detail}</span>
                </div>
                <div className="spacer" />
                {blockedOk ? (
                  <span className="badge badge-auto">Blocked instead</span>
                ) : (
                  o.link && (
                    <button className="btn-quiet btn-small" onClick={() => open(o)}>
                      Their page
                    </button>
                  )
                )}
              </div>
            ))}
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

        <div className="decide row">
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

/**
 * What happened, in the order that matters to the person reading it.
 *
 * Every clause is a thing that actually took effect. "Nothing to report" is
 * reserved for a run where genuinely nothing did — it used to appear after
 * blocking a sender, which was simply untrue.
 */
function summarise(r: {
  unsubscribed: number;
  blocked: number;
  binned: number;
  leftForYou: number;
}): string {
  const parts: string[] = [];
  if (r.unsubscribed > 0) parts.push(`${plural(r.unsubscribed, "sender")} unsubscribed`);
  if (r.blocked > 0) parts.push(`${plural(r.blocked, "sender")} blocked`);
  if (r.binned > 0) parts.push(`${plural(r.binned, "email")} binned`);
  if (parts.length === 0) {
    return r.leftForYou > 0
      ? `${plural(r.leftForYou, "sender")} couldn't be done automatically`
      : "Nothing to report";
  }
  const sentence = parts.join(", ").replace(/, ([^,]*)$/, parts.length > 2 ? ", and $1" : " and $1");
  return sentence.charAt(0).toUpperCase() + sentence.slice(1);
}
