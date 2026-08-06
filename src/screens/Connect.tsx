import { useState } from "react";

import * as api from "../api";
import { Notice } from "../components/ui";
import { errorCode, errorMessage, type Status } from "../types";

/**
 * The consent step.
 *
 * This asks for everything Hush can use, in one trip, and explains each one —
 * because Google's own consent screen presents them as separate checkboxes the
 * user can decline individually. Asking for two permissions here does not
 * impose two; it surfaces the choices in the place they are actually made.
 *
 * There were three. `gmail.send` went in 0.11.0: sending mail as somebody is
 * the largest thing this app could ask for, and it existed to reach about six
 * per cent of senders that blocking handles anyway.
 *
 * An earlier version asked only for read access and requested the rest later,
 * at the moment each was wanted. That reads well in principle and is worse in
 * practice: blocking is the recommended action, so the ordinary path collected
 * two extra trips through the browser mid-flow. The narrower option is still
 * here for anyone who wants it.
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

  async function go(everything: boolean) {
    setProblem(null);
    setBusy(true);
    try {
      onConnected(await api.connect(everything, everything));
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
          <h3>What Google will ask you to approve</h3>
          <p className="muted small">
            Two separate tick-boxes on Google's own page, on top of reading.
            You can decline either one there and Hush still works — it just does
            less.
          </p>
          <div className="stack stack-2">
            <span className="muted small">
              <strong style={{ color: "var(--ink)" }}>Read your mail</strong> —
              the sender, subject and date of each message. Never the contents.
              Hush cannot work without this one.
            </span>
            <span className="muted small">
              <strong style={{ color: "var(--ink)" }}>Manage your mail</strong> —
              so it can move a sender's old newsletters to Trash. It never
              deletes anything permanently, and cannot: this permission does not
              allow it.
            </span>
            <span className="muted small">
              <strong style={{ color: "var(--ink)" }}>Change your settings</strong>{" "}
              — so it can add a Gmail filter that blocks a sender for good. This
              is the one that makes "they still email me" impossible.
            </span>
            <span className="muted small">
              Hush never asks to <strong style={{ color: "var(--ink)" }}>send
              email as you</strong>, and never asks for the permission that
              would let it delete your mail permanently. Neither is on the list
              because neither is in the app.
            </span>
          </div>
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
          // Sticky, because explaining the permissions properly makes this
          // screen taller than a small window, and the button you came here to
          // press should never be somewhere you have to go looking for.
          <div className="decide stack stack-3">
            <div className="row">
              <button className="btn-quiet" onClick={onBack}>
                Back to setup
              </button>
              <div className="spacer" />
              <button className="btn-primary" onClick={() => go(true)} autoFocus>
                Connect with Google
              </button>
            </div>
            <button
              className="btn-quiet btn-small"
              style={{ alignSelf: "flex-end" }}
              onClick={() => go(false)}
            >
              Or connect with read-only access, and decide the rest later
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
