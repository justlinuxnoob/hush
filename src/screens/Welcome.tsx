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
              <strong style={{ color: "var(--ink)" }}>Delete anything.</strong>{" "}
              Not a single message. Hush only unsubscribes.
            </li>
            <li>
              <strong style={{ color: "var(--ink)" }}>Read your emails.</strong>{" "}
              It looks at who sent a message and its subject line. It never asks
              Google for what's inside.
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
          <h3>One catch, up front</h3>
          <p className="muted">
            To keep everything on your own computer, Hush uses your own Google
            connection rather than one of ours. Setting that up takes about five
            minutes of clicking through Google's website, once. We'll walk you
            through every step.
          </p>
        </div>

        <div className="row">
          <button className="btn-primary" onClick={onNext} autoFocus>
            Get started
          </button>
          <span className="muted small">Takes about five minutes</span>
        </div>
      </div>
    </div>
  );
}
