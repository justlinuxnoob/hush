import { useEffect, useState } from "react";

import * as api from "../api";
import { Meter, Notice, formatCount, plural } from "../components/ui";
import {
  errorCode,
  errorMessage,
  type BacklogAction,
  type BlockAction,
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
  // What a block does. Archiving unless the user says otherwise, every time.
  // The remembered preference only preselects — `acceptedTrash` below is the
  // second, explicit interaction that trashing always requires.
  const [blockAction, setBlockAction] = useState<BlockAction>(status.block_action);
  const [acceptedTrash, setAcceptedTrash] = useState(false);
  // The same question for the backlog. Archiving is the default here too: "get
  // this out of my inbox" and "delete this" are different wishes, and only one
  // of them is still reversible in a month.
  const [backlogAction, setBacklogAction] = useState<BacklogAction>(
    status.backlog_action
  );
  const [acceptedBacklogTrash, setAcceptedBacklogTrash] = useState(false);

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
          future !== "unsubscribe",
          blockAction,
          backlogAction
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
      const s = await api.connect(true, status.can_block);
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
      const s = await api.connect(status.can_delete, true);
      onStatusChange(s);
      if (s.can_block) setFuture("both");
    } catch (e) {
      if (errorCode(e) !== "cancelled") setProblem(errorMessage(e));
    } finally {
      setAsking(false);
    }
  }

  const blocking = future !== "unsubscribe" && status.can_block;
  // Choosing Trash is never enough on its own. The tick below it is the second
  // interaction, and until it happens the run button stays put.
  const needsTrashConsent = blocking && blockAction === "trash" && !acceptedTrash;
  const needsBacklogConsent =
    binBacklog && backlogAction === "trash" && !acceptedBacklogTrash;
  const blocked =
    (flagged.length > 0 && !acceptedFlagged) ||
    needsTrashConsent ||
    needsBacklogConsent;

  // Moving back to Archive has to clear the consent, or a later switch to Trash
  // would inherit a tick the user gave in a different context.
  function chooseBlockAction(next: BlockAction) {
    setBlockAction(next);
    if (next === "archive") setAcceptedTrash(false);
  }

  function chooseBacklogAction(next: BacklogAction) {
    setBacklogAction(next);
    if (next === "archive") setAcceptedBacklogTrash(false);
  }

  return (
    <div className="centre">
      <div className="inner stack stack-6">
        <div className="stack stack-3">
          <h1>Ready when you are</h1>
          <p className="lede">
            {plural(chosen.length, "sender")},{" "}
            {chosen.length === 1 ? "who has" : "who between them have"} sent you{" "}
            {formatCount(totalMail)} emails.
          </p>
        </div>

        <div className="card stack stack-4">
          <Group
            n={automatic.length}
            title="Unsubscribed automatically"
            detail="Hush tells the sender directly. Nothing for you to do."
            senders={automatic}
          />
          {/* Everything Hush cannot do on its own lands here, and the answer is
              always a filter rather than a list of links. There is no version
              of this screen that ends with the user having a job. */}
          <Group
            n={manual.length + byEmail.length}
            title="Blocked instead"
            detail={
              status.can_block
                ? "Nothing can be sent automatically for these, so a Gmail filter keeps their mail out of your inbox. Nothing for you to do."
                : "Nothing can be sent automatically for these. Allow Hush to block senders below and they're handled too."
            }
            senders={[...manual, ...byEmail]}
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

        <div className="card stack stack-4">
          <div className="stack">
            <h3>Stopping future emails</h3>
            <span className="muted small">
              Unsubscribing asks them to stop. Blocking is a rule in your own
              Gmail, so it doesn't depend on them.
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
                    ? "Both. Works whether or not they listen."
                    : "Needs Google's permission — the button below."}
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
                <span className="why">Proper, but they have to honour it.</span>
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
                    ? "Say nothing to them. Just stop it arriving."
                    : "Needs Google's permission."}
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

          {blocking && (
            <div className="stack stack-3" style={{ borderTop: "1px solid var(--rule)", paddingTop: "calc(var(--step) * 4)" }}>
              <span className="muted small">
                <strong style={{ color: "var(--ink)" }}>Where blocked mail goes.</strong>{" "}
                A block catches everything from that address, receipts included.
              </span>

              <div className="choices">
                <button
                  className="choice"
                  aria-pressed={blockAction === "archive"}
                  disabled={busy}
                  onClick={() => chooseBlockAction("archive")}
                >
                  <span>
                    <strong>Out of the inbox — recommended</strong>
                    <span className="why">
                      Kept in your account, searchable, never deleted.
                    </span>
                  </span>
                </button>
                <button
                  className="choice"
                  aria-pressed={blockAction === "trash"}
                  disabled={busy}
                  onClick={() => chooseBlockAction("trash")}
                  style={flagged.length > 0 ? { opacity: 0.62 } : undefined}
                >
                  <span>
                    <strong>Straight to Trash</strong>
                    <span className="why">
                      Tidier. Gmail empties Trash after 30 days.
                      {flagged.length > 0 && (
                        <>
                          {" "}
                          Not a good fit here — some of these look like they
                          send receipts.
                        </>
                      )}
                    </span>
                  </span>
                </button>
              </div>

              {blockAction === "trash" && (
                <Notice tone="caution">
                  <div className="stack stack-2">
                    <span>
                      Anything Gmail moves to Trash is <strong>permanently
                      deleted after 30 days</strong>, and Hush can't get it back
                      after that. That includes any order confirmation, receipt
                      or delivery note{" "}
                      {chosen.length === 1
                        ? "this sender sends"
                        : "these senders send"}{" "}
                      from the same address.
                    </span>
                    {flagged.length > 0 && (
                      <span>
                        {plural(flagged.length, "of the senders you picked looks", "of the senders you picked look")}{" "}
                        like they send that sort of mail. Keeping this on
                        "out of the inbox" would leave it recoverable.
                      </span>
                    )}
                    <label
                      className="row"
                      style={{ marginBottom: 0, cursor: "pointer", alignItems: "flex-start" }}
                    >
                      <input
                        type="checkbox"
                        checked={acceptedTrash}
                        onChange={(e) => setAcceptedTrash(e.target.checked)}
                        style={{ marginTop: "5px", accentColor: "var(--caution)" }}
                      />
                      <span style={{ fontWeight: 500 }}>
                        I understand, send their mail to Trash
                      </span>
                    </label>
                  </div>
                </Notice>
              )}

              <span className="muted small">
                Undo any block later under Blocked senders.
              </span>
            </div>
          )}
        </div>

        {binnable > 0 && (
          <div className="card stack stack-3">
            <div className="stack">
              <h3>Their old emails</h3>
              <span className="muted small">
                {formatCount(binnable)} of theirs are already in your inbox.
                {kept > 0 && (
                  <>
                    {" "}
                    {formatCount(kept)} more — receipts and the like — are left
                    alone either way.
                  </>
                )}
              </span>
            </div>

            {status.can_delete ? (
              <>
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
                    Clear their {formatCount(binnable)} old newsletters out of
                    the inbox
                  </strong>
                  <span className="muted small" style={{ display: "block", fontWeight: 400 }}>
                    Only newsletters Hush has scanned. Receipts stay put.
                  </span>
                </span>
              </label>

              {binBacklog && (
                <div className="choices">
                  <button
                    className="choice"
                    aria-pressed={backlogAction === "archive"}
                    disabled={busy}
                    onClick={() => chooseBacklogAction("archive")}
                  >
                    <span>
                      <strong>Archive them — recommended</strong>
                      <span className="why">
                        Out of the inbox, kept in your account under a{" "}
                        <span className="mono">Hush</span> label. Never deleted.
                      </span>
                    </span>
                  </button>
                  <button
                    className="choice"
                    aria-pressed={backlogAction === "trash"}
                    disabled={busy}
                    onClick={() => chooseBacklogAction("trash")}
                  >
                    <span>
                      <strong>Move them to Trash</strong>
                      <span className="why">
                        Gone from the account. Recoverable for 30 days, then
                        Gmail deletes them.
                      </span>
                    </span>
                  </button>
                </div>
              )}

              {binBacklog && backlogAction === "trash" && (
                <Notice tone="caution">
                  <div className="stack stack-2">
                    <span>
                      Gmail <strong>permanently deletes</strong> trashed mail
                      after 30 days. Archiving does the same job to your inbox
                      and keeps the mail.
                    </span>
                    <label
                      className="row"
                      style={{ marginBottom: 0, cursor: "pointer", alignItems: "flex-start" }}
                    >
                      <input
                        type="checkbox"
                        checked={acceptedBacklogTrash}
                        onChange={(e) => setAcceptedBacklogTrash(e.target.checked)}
                        style={{ marginTop: "5px", accentColor: "var(--caution)" }}
                      />
                      <span style={{ fontWeight: 500 }}>
                        I understand, move them to Trash
                      </span>
                    </label>
                  </div>
                </Notice>
              )}
              </>
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

        <div className="row decide">
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
                    ? `${backlogAction === "trash" ? "bin" : "archive"} ${plural(
                        binnable,
                        "email"
                      )}`
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
                ? `${formatCount(progress.done)} of ${formatCount(progress.total)} emails moved`
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
