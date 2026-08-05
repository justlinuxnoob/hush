import { useEffect, useState } from "react";

import * as api from "../api";
import { Notice, formatDate } from "../components/ui";
import { errorCode, errorMessage, type Status } from "../types";

export default function Settings({
  status,
  onStatus,
  onClose,
  onReset,
}: {
  status: Status;
  onStatus: (s: Status) => void;
  onClose: () => void;
  onReset: () => void;
}) {
  const [where, setWhere] = useState("");
  const [confirmErase, setConfirmErase] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api.dataLocation().then(setWhere).catch(() => setWhere(""));
  }, []);

  async function guard(work: () => Promise<Status | void>) {
    setProblem(null);
    setBusy(true);
    try {
      const s = await work();
      onStatus(s ?? (await api.status()));
    } catch (e) {
      // Backing out of Google's page is a choice, not a failure.
      if (errorCode(e) !== "cancelled") setProblem(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  async function erase() {
    setProblem(null);
    setBusy(true);
    try {
      onStatus(await api.eraseEverything());
      onReset();
    } catch (e) {
      setProblem(errorMessage(e));
      setBusy(false);
    }
  }

  return (
    <div className="centre narrow">
      <div className="inner stack stack-8">
        <div className="row">
          <h1>Settings</h1>
          <div className="spacer" />
          <button className="btn-quiet" onClick={onClose}>
            Close
          </button>
        </div>

        {problem && <Notice tone="problem">{problem}</Notice>}

        <div className="stack stack-4">
          <h3>Your account</h3>
          <div className="card stack stack-3">
            <div className="row">
              <span>{status.email ?? "Not connected"}</span>
              <div className="spacer" />
              <span className={`badge ${status.connected ? "badge-auto" : "badge-neutral"}`}>
                {status.connected ? "Connected" : "Not connected"}
              </span>
            </div>
            {status.scan_complete && (
              <span className="muted small">
                Last looked through your mail on {formatDate(status.last_scan_ms)} —{" "}
                {status.message_count.toLocaleString()} messages,{" "}
                {status.sender_count.toLocaleString()} senders you can unsubscribe from.
              </span>
            )}
            {status.token_storage === "memory" && (
              <Notice tone="caution">
                This computer has no working password store, so you'll need to
                connect again after quitting.
              </Notice>
            )}
          </div>
        </div>

        <div className="stack stack-4">
          <h3>How Hush behaves</h3>

          <div className="card stack stack-4">
            <div className="stack stack-3">
              <div className="stack">
                <strong>Unsubscribes that work by email</strong>
                <span className="muted small">
                  A few senders only accept an email. Choose who sends it.
                </span>
              </div>
              <div className="choices">
                <ModeChoice
                  active={status.mailto_mode === "hand_off"}
                  title="Open my mail app"
                  why="Hush writes the message, you press send. No extra permission needed."
                  onPick={() => guard(() => api.setMailtoMode("hand_off"))}
                  disabled={busy}
                />
                <ModeChoice
                  active={status.mailto_mode === "send_via_gmail"}
                  title="Let Hush send it"
                  why={
                    status.can_send
                      ? "Fully automatic. Hush sends a short message from your account."
                      : "Fully automatic. Google has to grant this separately, so choosing it opens your browser once."
                  }
                  // If the permission is missing, ask for it rather than
                  // disabling the option and telling the user to go and find it.
                  onPick={() =>
                    guard(async () => {
                      if (!status.can_send) {
                        const s = await api.connect(true, status.can_delete);
                        if (!s.can_send) return s;
                        await api.setMailtoMode("send_via_gmail");
                        return await api.status();
                      }
                      return api.setMailtoMode("send_via_gmail");
                    })
                  }
                  disabled={busy}
                />
              </div>
            </div>
          </div>
        </div>

        <div className="stack stack-4">
          <h3>Your data</h3>
          <div className="card stack stack-3">
            <p className="muted small">
              Everything Hush knows lives in one file on this computer. It holds
              who sent you what, when, the subject lines, and your own choices —
              never the contents of any message.
            </p>
            {where && (
              <input type="text" readOnly value={where} className="mono" aria-label="Where the file is" />
            )}
          </div>
        </div>

        <div className="stack stack-4">
          <h3>Ending things</h3>
          <div className="card stack stack-4">
            <div className="row" style={{ alignItems: "flex-start" }}>
              <div className="stack" style={{ flex: 1 }}>
                <strong>Disconnect</strong>
                <span className="muted small">
                  Ends Hush's access to your Google account and forgets the
                  connection. Your local list stays.
                </span>
              </div>
              <button
                className="btn-secondary"
                disabled={busy || !status.connected}
                onClick={() => guard(() => api.disconnect(false))}
              >
                Disconnect
              </button>
            </div>

            <hr className="rule" />

            <div className="stack stack-3">
              <div className="stack">
                <strong>Disconnect and erase everything</strong>
                <span className="muted small">
                  Removes Hush's access, deletes the stored connection, and
                  deletes the local file entirely. Your email is untouched — this
                  only clears what's on this computer.
                </span>
              </div>
              {!confirmErase ? (
                <button
                  className="btn-warm"
                  style={{ alignSelf: "flex-start" }}
                  onClick={() => setConfirmErase(true)}
                  disabled={busy}
                >
                  Erase everything
                </button>
              ) : (
                <div className="row">
                  <button className="btn-warm" onClick={erase} disabled={busy}>
                    {busy ? "Erasing…" : "Yes, erase it all"}
                  </button>
                  <button className="btn-quiet" onClick={() => setConfirmErase(false)}>
                    Cancel
                  </button>
                </div>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function ModeChoice({
  active,
  title,
  why,
  onPick,
  disabled,
}: {
  active: boolean;
  title: string;
  why: string;
  onPick: () => void;
  disabled?: boolean;
}) {
  return (
    <button className="choice" aria-pressed={active} onClick={onPick} disabled={disabled}>
      <span>
        <strong>{title}</strong>
        <span className="why">{why}</span>
      </span>
    </button>
  );
}
