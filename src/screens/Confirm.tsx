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
  // Two independent questions: what happens to future mail, and what happens
  // to the pile already sitting there. Double protection is the default,
  // because asking a sender to stop and making sure they do are different
  // things and most people want both.
  // Defaults to the recommended pairing, but only when blocking is actually
  // available — otherwise the button would promise something it cannot do.
  const [future, setFuture] = useState<"unsubscribe" | "block" | "both">(
    status.can_block ? "both" : "unsubscribe"
  );
  const [binBacklog, setBinBacklog] = useState(false);
  const [asking, setAsking] = useState(false);

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

  // Granting the permission mid-flow should move the selection to the option it
  // just unlocked, rather than leaving a recommendation the user cannot pick.
  useEffect(() => {
    if (status.can_block && future === "unsubscribe") setFuture("both");
    if (!status.can_block && future !== "unsubscribe") setFuture("unsubscribe");
    // Only when the permission changes; not when the user picks something else.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status.can_block]);

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
          future !== "block",
          binBacklog,
          future !== "unsubscribe"
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
      if (s.can_delete) setBinBacklog(true);
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
      if (s.can_block) setFuture("both");
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

        {binnable > 0 && (
          <div className="card stack stack-3">
            <div className="stack">
              <h3>Their old emails</h3>
              <span className="muted small">
                {binnable.toLocaleString()} newsletters from these senders are
                already in your inbox. Stopping future mail does nothing about
                those.
                {kept > 0 && (
                  <>
                    {" "}
                    Their {kept.toLocaleString()} other{" "}
                    {kept === 1 ? "email" : "emails"} — receipts, confirmations
                    and the like — are left alone either way.
                  </>
                )}{" "}
                Only emails Hush has scanned can be moved.
              </span>
            </div>

            {status.can_delete ? (
              <label
                className="row"
                style={{ marginBottom: 0, cursor: "pointer", alignItems: "flex-start" }}
              >
                <input
                  type="checkbox"
                  checked={binBacklog}
                  onChange={(e) => setBinBacklog(e.target.checked)}
                  style={{ marginTop: "5px", accentColor: "var(--accent)" }}
                />
                <span>
                  <strong>
                    Move their {binnable.toLocaleString()} old newsletters to Trash
                  </strong>
                  <span className="muted small" style={{ display: "block", fontWeight: 400 }}>
                    Recoverable there for 30 days.
                  </span>
                </span>
              </label>
            ) : (
              <div>
                <button
                  className="btn-secondary"
                  onClick={askToBin}
                  disabled={asking || busy}
                >
                  {asking ? "Waiting for your browser…" : "Allow Hush to do that"}
                </button>
              </div>
            )}
          </div>
        )}

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

        <div className="card stack stack-4">
          <div className="stack">
            <h3>Stopping future emails</h3>
            <span className="muted small">
              Unsubscribing asks the sender to stop and depends on them doing
              it. Blocking is a filter in your own Gmail that doesn't ask
              anyone. Together, one is polite and the other is certain.
            </span>
          </div>

          <div className="choices">
            <button
              className="choice"
              aria-pressed={future === "both"}
              disabled={busy || !status.can_block}
              onClick={() => setFuture("both")}
            >
              <span>
                <strong>Unsubscribe and block — recommended</strong>
                <span className="why">
                  {status.can_block
                    ? "Ask them to stop, and send anything they do send to Trash anyway. This is the combination that actually works."
                    : "Needs Google's permission for filters. Grant it below and this becomes available."}
                </span>
              </span>
            </button>
            <button
              className="choice"
              aria-pressed={future === "unsubscribe"}
              disabled={busy}
              onClick={() => setFuture("unsubscribe")}
            >
              <span>
                <strong>Unsubscribe only</strong>
                <span className="why">
                  The polite version. Takes you off their list properly — but if
                  they ignore it, or take a fortnight, the mail keeps arriving.
                </span>
              </span>
            </button>
            <button
              className="choice"
              aria-pressed={future === "block"}
              disabled={busy || !status.can_block}
              onClick={() => setFuture("block")}
            >
              <span>
                <strong>Block only</strong>
                <span className="why">
                  {status.can_block
                    ? "Never contact them; just stop their mail reaching you. For senders you'd rather not tell you're leaving."
                    : "Needs Google's permission for filters."}
                </span>
              </span>
            </button>
          </div>

          {!status.can_block && (
            <div>
              <button
                className="btn-secondary"
                onClick={askToBlock}
                disabled={asking || busy}
              >
                {asking ? "Waiting for your browser…" : "Allow Hush to block senders"}
              </button>
            </div>
          )}
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
              : [
                  future === "block"
                    ? `Block ${plural(chosen.length, "sender")}`
                    : future === "both"
                      ? `Unsubscribe and block ${plural(chosen.length, "sender")}`
                      : `Unsubscribe from ${plural(chosen.length, "sender")}`,
                  binBacklog && binnable > 0
                    ? `bin ${plural(binnable, "email")}`
                    : null,
                ]
                  .filter(Boolean)
                  .join(", ")}
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
