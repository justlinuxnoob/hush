import { useEffect, useState } from "react";

import * as api from "../api";
import { Notice, plural } from "../components/ui";
import {
  errorMessage,
  type PlannedAction,
  type RunReport,
  type Sender,
  type Status,
} from "../types";

/**
 * The last screen before anything happens.
 *
 * It says exactly who, exactly how many, and exactly what will be sent. Flagged
 * senders need a separate, explicit confirmation — a single "yes" should not
 * cover both a shop newsletter and something that looks like a bank.
 */
export default function Confirm({
  status,
  senders,
  addresses,
  onDone,
  onBack,
}: {
  status: Status;
  senders: Sender[];
  addresses: string[];
  onDone: (report: RunReport) => void;
  onBack: () => void;
}) {
  const [plan, setPlan] = useState<PlannedAction[] | null>(null);
  const [showDetail, setShowDetail] = useState(false);
  const [acceptedFlagged, setAcceptedFlagged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  const chosen = senders.filter((s) => addresses.includes(s.address));
  const flagged = chosen.filter((s) => s.assessment.caution);
  const automatic = chosen.filter((s) => s.method.kind === "one_click");
  const byEmail = chosen.filter((s) => s.method.kind === "mailto");
  const manual = chosen.filter((s) => s.method.kind === "manual_link");
  const totalMail = chosen.reduce((n, s) => n + s.message_count, 0);

  useEffect(() => {
    api
      .planUnsubscribe(addresses)
      .then(setPlan)
      .catch((e) => setProblem(errorMessage(e)));
  }, [addresses]);

  async function go() {
    setProblem(null);
    setBusy(true);
    try {
      onDone(await api.runUnsubscribe(addresses));
    } catch (e) {
      setProblem(errorMessage(e));
      setBusy(false);
    }
  }

  const blocked = flagged.length > 0 && !acceptedFlagged;

  return (
    <div className="centre">
      <div className="inner stack stack-6">
        <div className="stack stack-3">
          <h1>Ready when you are</h1>
          <p className="lede">
            {status.dry_run
              ? "Dry run is on, so this is a rehearsal. Nothing will be sent."
              : `Unsubscribing from ${plural(
                  chosen.length,
                  "sender"
                )} who've sent you ${totalMail.toLocaleString()} emails.`}
          </p>
        </div>

        {status.dry_run && (
          <Notice tone="accent">
            Hush will show you precisely what it would do and send nothing at
            all. Turn dry run off on the previous screen when you're ready for
            real.
          </Notice>
        )}

        <div className="card stack stack-4">
          <Group
            n={automatic.length}
            title="Unsubscribed automatically"
            detail="Hush tells the sender directly. Nothing for you to do."
            senders={automatic}
          />
          <Group
            n={byEmail.length}
            title={
              status.mailto_mode === "send_via_gmail"
                ? "Unsubscribed by email"
                : "A ready-written email opens"
            }
            detail={
              status.mailto_mode === "send_via_gmail"
                ? "Hush sends a short unsubscribe message from your account."
                : "Your own mail app opens with the message written. You press send."
            }
            senders={byEmail}
          />
          <Group
            n={manual.length}
            title="You'll open these yourself"
            detail="These senders only offer a link. Hush won't click it for you — it'll list them so you can."
            senders={manual}
          />
        </div>

        {flagged.length > 0 && (
          <div className="notice notice-caution stack stack-3">
            <strong>
              {plural(flagged.length, "sender")} here{" "}
              {flagged.length === 1 ? "looks" : "look"} like{" "}
              {flagged.length === 1 ? "it sends" : "they send"} things you might
              need
            </strong>
            <div className="stack stack-2">
              {flagged.map((s) => (
                <div key={s.address} className="small">
                  <strong>{s.display_name}</strong> — {s.assessment.reasons.join(". ")}
                </div>
              ))}
            </div>
            <label
              className="row"
              style={{ marginBottom: 0, cursor: "pointer", alignItems: "flex-start" }}
            >
              <input
                type="checkbox"
                checked={acceptedFlagged}
                onChange={(e) => setAcceptedFlagged(e.target.checked)}
                style={{ marginTop: "5px", accentColor: "var(--caution)" }}
              />
              <span style={{ fontWeight: 500 }}>
                I've checked these and I still want to unsubscribe
              </span>
            </label>
          </div>
        )}

        <div className="stack stack-3">
          <button className="btn-quiet btn-small" onClick={() => setShowDetail((v) => !v)} style={{ alignSelf: "flex-start" }}>
            {showDetail ? "Hide the exact details" : "Show me exactly what gets sent"}
          </button>
          {showDetail && (
            <div className="stack stack-3">
              {plan === null && <span className="muted small">Working it out…</span>}
              {plan?.map((p) => (
                <div key={p.address}>
                  <div className="small">
                    <strong>{p.display_name}</strong> — {p.what}
                  </div>
                  <div className="plan-detail">{p.detail}</div>
                </div>
              ))}
            </div>
          )}
        </div>

        {problem && <Notice tone="problem">{problem}</Notice>}

        <div className="row">
          <button className="btn-quiet" onClick={onBack} disabled={busy}>
            Back
          </button>
          <div className="spacer" />
          <button className="btn-primary" onClick={go} disabled={busy || blocked}>
            {busy
              ? "Working…"
              : status.dry_run
                ? "Run the rehearsal"
                : `Unsubscribe from ${plural(chosen.length, "sender")}`}
          </button>
        </div>

        {blocked && (
          <p className="muted small">
            Tick the box above to continue, or go back and deselect those senders.
          </p>
        )}
      </div>
    </div>
  );
}

function Group({
  n,
  title,
  detail,
  senders,
}: {
  n: number;
  title: string;
  detail: string;
  senders: Sender[];
}) {
  if (n === 0) return null;
  return (
    <div className="stack stack-2">
      <div className="row row-tight">
        <strong className="tabular">{n}</strong>
        <strong>{title}</strong>
      </div>
      <span className="muted small">{detail}</span>
      <span className="muted small">
        {senders
          .slice(0, 6)
          .map((s) => s.display_name)
          .join(", ")}
        {senders.length > 6 && ` and ${senders.length - 6} more`}
      </span>
    </div>
  );
}
