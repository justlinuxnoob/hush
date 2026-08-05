import { useState } from "react";

import * as api from "../api";
import { Notice } from "../components/ui";
import { errorCode, errorMessage, type Status } from "../types";

/**
 * The consent step, and nothing else.
 *
 * It asks for read-only access and stops there. The wider permissions — binning
 * old mail, sending unsubscribe emails — are deliberately *not* offered here:
 * at this point nobody has seen a single sender, so there is no way to make an
 * informed choice, and answering "no" would mean coming back to reconnect.
 * They're asked for later, in the moment they are actually wanted.
 */
export default function Connect({
  status,
  onConnected,
  onBack,
}: {
  status: Status;
  onConnected: (s: Status) => void;
  onBack: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  async function go() {
    setProblem(null);
    setBusy(true);
    try {
      onConnected(await api.connect(false, false, false));
    } catch (e) {
      // Giving up is a choice, not a failure, so it earns no error message.
      if (errorCode(e) !== "cancelled") setProblem(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  async function giveUp() {
    try {
      await api.cancelConnect();
    } catch {
      // Nothing useful to say; the wait is ending either way.
    }
  }

  return (
    <div className="centre narrow">
      <div className="inner stack stack-6">
        <div className="stack stack-3">
          <h1>Connect your Gmail</h1>
          <p className="lede">
            Your browser will open on Google's sign-in page. Hush never sees
            your password — Google hands back a pass that only works for reading
            who sent you what.
          </p>
        </div>

        <div className="panel stack stack-3">
          <h3>What Hush is asking for</h3>
          <p className="muted">
            <strong style={{ color: "var(--ink)" }}>
              Permission to read your mail.
            </strong>{" "}
            Hush uses it to fetch the sender, subject and date of each message —
            never the message itself.
          </p>
          <p className="muted">
            That is all it asks for. It cannot delete, move or send anything with
            this.
          </p>
        </div>

        {!status.keychain_available && (
          <Notice tone="caution">
            This computer doesn't have a working password store, so the
            connection will only last until you quit Hush. Everything still
            works — you'll just sign in again next time.
          </Notice>
        )}

        <Notice tone="calm">
          Because your Google project is in Testing mode, the connection ends
          after seven days and Hush will ask you to reconnect. That's Google's
          rule, not ours.
        </Notice>

        {problem && <Notice tone="problem">{problem}</Notice>}

        {busy ? (
          <div className="stack stack-4">
            <div className="row">
              <span className="spinner" aria-hidden="true" />
              <span>Waiting for you to finish in the browser…</span>
            </div>
            <p className="muted small">
              Nothing happening? The tab may have been closed, or opened in a
              window you can't see. Stop waiting and try again.
            </p>
            <div>
              <button className="btn-secondary" onClick={giveUp}>
                Stop waiting
              </button>
            </div>
          </div>
        ) : (
          <div className="row">
            <button className="btn-quiet" onClick={onBack}>
              Back to setup
            </button>
            <div className="spacer" />
            <button className="btn-primary" onClick={go} autoFocus>
              Connect with Google
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
