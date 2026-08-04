import { useState } from "react";

import * as api from "../api";
import { Notice, Screenshot } from "../components/ui";
import { errorMessage, type Status } from "../types";

/**
 * The consent step.
 *
 * The wider "send mail" permission is explained in full and left switched off.
 * Nobody should discover after the fact that an app can send mail as them.
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
  const [allowSend, setAllowSend] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  async function go() {
    setProblem(null);
    setBusy(true);
    try {
      onConnected(await api.connect(allowSend));
    } catch (e) {
      setProblem(errorMessage(e));
    } finally {
      setBusy(false);
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
            <strong style={{ color: "var(--ink)" }}>Read your mail.</strong> Hush
            uses this to fetch the sender, subject and date of each message —
            never the message itself.
          </p>
          <p className="muted small">
            Google words this permission broadly on its own screen. Hush's use of
            it is narrow, and the code that proves it is one file:{" "}
            <span className="mono">src-tauri/src/gmail.rs</span>.
          </p>
        </div>

        <div className="card stack stack-3">
          <label
            className="row"
            style={{ marginBottom: 0, cursor: "pointer", alignItems: "flex-start" }}
          >
            <input
              type="checkbox"
              checked={allowSend}
              onChange={(e) => setAllowSend(e.target.checked)}
              style={{ marginTop: "5px", accentColor: "var(--accent)" }}
            />
            <span>
              <strong>Also let Hush send mail as me</strong>
              <span className="muted small" style={{ display: "block", fontWeight: 400 }}>
                A few senders only accept unsubscribes by email. With this off —
                which is the sensible default — Hush opens a ready-written
                message in your own mail app and you press send. With it on, Hush
                sends those directly. It's a much bigger permission, and you can
                change your mind later.
              </span>
            </span>
          </label>
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

        <Screenshot describe="Google's consent screen showing the Hush app name and the read-only Gmail permission" />

        <div className="row">
          <button className="btn-quiet" onClick={onBack} disabled={busy}>
            Back to setup
          </button>
          <div className="spacer" />
          <button className="btn-primary" onClick={go} disabled={busy} autoFocus>
            {busy ? "Waiting for your browser…" : "Connect with Google"}
          </button>
        </div>

        {busy && (
          <p className="muted small">
            Finished in the browser but nothing happened here? Close that tab and
            press Connect again.
          </p>
        )}
      </div>
    </div>
  );
}
