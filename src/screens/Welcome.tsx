import { Wordmark } from "../components/ui";

/**
 * The first screen. Its whole job is to set expectations honestly, including
 * the limits — someone who reads this and decides Hush is not for them has
 * been served well.
 */
export default function Welcome({ onNext }: { onNext: () => void }) {
  return (
    <div className="centre">
      <div className="inner stack stack-8">
        <div className="stack stack-4">
          <Wordmark />
          <h1>Fewer emails, without losing the ones you need.</h1>
          <p className="lede">
            Hush finds everyone who sends you bulk email, shows you how much
            each one sends, and unsubscribes from the ones you pick.
          </p>
        </div>

        <div className="stack stack-4">
          <h3>What it will never do</h3>
          <ul className="stack stack-2 muted" style={{ paddingLeft: "1.1rem", margin: 0 }}>
            <li>
              <strong style={{ color: "var(--ink)" }}>
                Delete anything you didn't ask it to.
              </strong>{" "}
              Unsubscribing is all it does by default. If you want a sender's old
              newsletters gone as well, you tick a box — and they go to your
              Gmail Trash, where you can pull them back for 30 days. Nothing is
              ever destroyed.
            </li>
            <li>
              <strong style={{ color: "var(--ink)" }}>Read your emails.</strong>{" "}
              It looks at who sent a message and its subject line. It never asks
              Google for what's inside, and there is no permission it holds that
              would let it.
            </li>
            <li>
              <strong style={{ color: "var(--ink)" }}>
                Touch receipts and codes.
              </strong>{" "}
              Order confirmations, sign-in codes and password resets don't carry
              an unsubscribe option, and Hush only ever offers you senders that
              do.
            </li>
            <li>
              <strong style={{ color: "var(--ink)" }}>Send anything anywhere.</strong>{" "}
              There's no Hush server. Your email never leaves this computer.
              Nothing is measured, counted, or reported back.
            </li>
          </ul>
        </div>

        <div className="panel stack stack-2">
          <h3>How the setup works</h3>
          <p className="muted">
            Because there's no Hush server, you connect Hush to Gmail directly
            rather than through us. That means clicking through a few pages on
            Google's site once — Hush opens each one for you, tells you which
            button to press, and you paste one file back in at the end.
          </p>
        </div>

        <div className="decide row">
          <button className="btn-primary" onClick={onNext} autoFocus>
            Get started
          </button>
          <span className="muted small">A couple of minutes, once</span>
        </div>
      </div>
    </div>
  );
}
