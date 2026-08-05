import { useEffect, useState } from "react";

import * as api from "../api";
import { Meter, Notice, plural } from "../components/ui";
import {
  errorCode,
  errorMessage,
  type PlannedAction,
  type RunProgress,
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
  onStatusChange,
}: {
  status: Status;
  senders: Sender[];
  addresses: string[];
  onDone: (report: RunReport) => void;
  onBack: () => void;
  onStatusChange: (s: Status) => void;
}) {
  const [plan, setPlan] = useState<PlannedAction[] | null>(null);
  const [showDetail, setShowDetail] = useState(false);
  const [acceptedFlagged, setAcceptedFlagged] = useState(false);
  const [action, setAction] = useState<"unsubscribe" | "bin" | "both">("unsubscribe");
  const [asking, setAsking] = useState(false);
  const [alsoBlock, setAlsoBlock] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [progress, setProgress] = useState<RunProgress | null>(null);

  const chosen = senders.filter((s) => addresses.includes(s.address));
  const flagged = chosen.filter((s) => s.assessment.caution);
  const automatic = chosen.filter((s) => s.method.kind === "one_click");
  const byEmail = chosen.filter((s) => s.method.kind === "mailto");
  const manual = chosen.filter((s) => s.method.kind === "manual_link");
  const totalMail = chosen.reduce((n, s) => n + s.message_count, 0);
  const binnable = chosen.reduce((n, s) => n + s.bulk_count, 0);
  const kept = totalMail - binnable;

  useEffect(() => api.onRunProgress(setProgress), []);

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
      onDone(
        await api.runUnsubscribe(
          addresses,
          action !== "bin",
          action !== "unsubscribe",
          alsoBlock
        )
      );
    } catch (e) {
      setProblem(errorMessage(e));
      setBusy(false);
    }
  }

  /**
   * Ask Google for the tidy-up permission, here and now.
   *
   * This is why the connect screen no longer asks: by this point the user has
   * seen the senders, knows how many emails are involved, and is choosing with
   * the numbers in front of them.
   */
  async function askToBin() {
    setProblem(null);
    setAsking(true);
    try {
      const s = await api.connect(status.can_send, true, status.can_block);
      onStatusChange(s);
      // They asked for it, so preselect the thing they asked for.
      if (s.can_delete) setAction("both");
    } catch (e) {
      if (errorCode(e) !== "cancelled") setProblem(errorMessage(e));
    } finally {
      setAsking(false);
    }
  }


  /** Ask for the filter permission at the moment the guarantee is wanted. */
  async function askToBlock() {
    setProblem(null);
    setAsking(true);
    try {
      const s = await api.connect(status.can_send, status.can_delete, true);
      onStatusChange(s);
      if (s.can_block) setAlsoBlock(true);
    } catch (e) {
      if (errorCode(e) !== "cancelled") setProblem(errorMessage(e));
    } finally {
      setAsking(false);
    }
  }

  const blocked = flagged.length > 0 && !acceptedFlagged;

  return (
    <div className="centre">
      <div className="inner stack stack-6">
        <div className="stack stack-3">
          <h1>Ready when you are</h1>
          <p className="lede">
            {plural(chosen.length, "sender")}, who between them have sent you{" "}
            {totalMail.toLocaleString()} emails.
          </p>
        </div>

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

        {!status.can_delete && binnable > 0 && (
          <div className="card stack stack-3">
            <div className="stack">
              <strong>Want their old emails gone too?</strong>
              <span className="muted small">
                {binnable.toLocaleString()} newsletters from these senders could
                move to your Gmail Trash, recoverable there for 30 days.
                {kept > 0 && (
                  <>
                    {" "}
                    Their {kept.toLocaleString()} other{" "}
                    {kept === 1 ? "email" : "emails"} — receipts, confirmations
                    and the like — would be left alone.
                  </>
                )}{" "}
                Only emails Hush has scanned can be moved. Google grants this
                separately, so your browser will open once.
              </span>
            </div>
            <div>
              <button
                className="btn-secondary"
                onClick={askToBin}
                disabled={asking || busy}
              >
                {asking ? "Waiting for your browser…" : "Allow this"}
              </button>
            </div>
          </div>
        )}

        <div className="card stack stack-3">
          <div className="stack">
            <strong>Make sure they actually stop</strong>
            <span className="muted small">
              Unsubscribing asks a sender to stop, and most do within a few
              days — but it depends entirely on them. A Gmail filter doesn't
              ask: anything they send from now on goes straight to your Trash,
              whether they honour the unsubscribe or not.
            </span>
          </div>

          {status.can_block ? (
            <label
              className="row"
              style={{ marginBottom: 0, cursor: "pointer", alignItems: "flex-start" }}
            >
              <input
                type="checkbox"
                checked={alsoBlock}
                onChange={(e) => setAlsoBlock(e.target.checked)}
                style={{ marginTop: "5px", accentColor: "var(--accent)" }}
              />
              <span>
                <strong>
                  Also block future emails from{" "}
                  {plural(chosen.length, "this sender", "these senders")}
                </strong>
                <span className="muted small" style={{ display: "block", fontWeight: 400 }}>
                  Creates a Gmail filter you can see and remove at any time under
                  Settings → Filters in Gmail. Nothing is permanently deleted —
                  it goes to Trash, same as everything else here.
                </span>
              </span>
            </label>
          ) : (
            <div>
              <button
                className="btn-secondary"
                onClick={askToBlock}
                disabled={asking || busy}
              >
                {asking ? "Waiting for your browser…" : "Let Hush do that"}
              </button>
            </div>
          )}
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

        <div className="card stack stack-3">
          <h3>What should Hush do?</h3>
          <div className="choices">
            <button
              className="choice"
              aria-pressed={action === "unsubscribe"}
              disabled={busy}
              onClick={() => setAction("unsubscribe")}
            >
              <span>
                <strong>Unsubscribe only</strong>
                <span className="why">
                  Stop what arrives next. Everything already in your inbox stays
                  exactly where it is.
                </span>
              </span>
            </button>
            <button
              className="choice"
              aria-pressed={action === "bin"}
              disabled={busy || !status.can_delete}
              onClick={() => setAction("bin")}
            >
              <span>
                <strong>Bin their old emails only</strong>
                <span className="why">
                  {status.can_delete
                    ? `Move ${binnable.toLocaleString()} old newsletters to Trash and leave the subscription alone — for senders you still want to hear from.`
                    : "Needs Google's permission to move mail. Allow it below first."}
                </span>
              </span>
            </button>
            <button
              className="choice"
              aria-pressed={action === "both"}
              disabled={busy || !status.can_delete}
              onClick={() => setAction("both")}
            >
              <span>
                <strong>Both</strong>
                <span className="why">
                  {status.can_delete
                    ? `Unsubscribe, and move their ${binnable.toLocaleString()} old newsletters to Trash.`
                    : "Needs Google's permission to move mail. Allow it below first."}
                </span>
              </span>
            </button>
          </div>
        </div>

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
          {busy ? (
            <button className="btn-secondary" onClick={() => api.cancelRun()}>
              Stop
            </button>
          ) : (
            <button className="btn-quiet" onClick={onBack}>
              Back
            </button>
          )}
          <div className="spacer" />
          <button className="btn-primary" onClick={go} disabled={busy || blocked}>
            {busy
              ? progress
                ? `${progress.doing}…`
                : "Starting…"
              : action === "bin"
                ? `Bin ${plural(binnable, "email")}`
                : action === "both"
                  ? `Unsubscribe and bin ${plural(binnable, "email")}`
                  : `Unsubscribe from ${plural(chosen.length, "sender")}`}
          </button>
        </div>

        {busy && progress && progress.total > 0 && (
          <div className="stack stack-2">
            <Meter value={progress.done} max={progress.total} />
            <span className="muted small tabular">
              {progress.binning
                ? `${progress.done.toLocaleString()} of ${progress.total.toLocaleString()} emails moved`
                : `${progress.done} of ${progress.total} senders`}
            </span>
          </div>
        )}

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
