import { Wordmark } from "../components/ui";

/**
 * The first screen, and the one that decides whether anyone sees the second.
 *
 * It used to be four dense paragraphs under the heading "What it will never
 * do" — every claim true, and read as a wall of text opening with a list of
 * negatives before the app had said what it was for. Seen properly only once
 * it was screenshotted rather than read in source.
 *
 * The promises are the differentiator and they stay. What changed is their
 * shape: one line each in a grid you can take in at a glance, instead of four
 * paragraphs you have to work through. The detail behind each one lives in the
 * README, where someone who wants it will go looking.
 */
export default function Welcome({ onNext }: { onNext: () => void }) {
  return (
    <div className="centre">
      <div className="inner stack stack-6">
        <div className="stack stack-3">
          <Wordmark />
          <h1>Fewer emails, without losing the ones you need.</h1>
          <p className="lede">
            It finds everyone who mails you in bulk and unsubscribes from the
            ones you pick.
          </p>
        </div>

        {/* Titles only.
            With a sentence of body each, four promises are four paragraphs —
            and on a phone the two-column grid collapses to one, so it became
            the wall of text it was meant to replace. The titles alone are the
            pitch; anyone who wants the reasoning has the README and, more to
            the point, the app's actual behaviour. */}
        <ul className="promises">
          <li>Never deletes your mail</li>
          <li>Never reads what's inside</li>
          <li>Never touches receipts</li>
          <li>Never sends anything anywhere</li>
        </ul>

        <p className="muted small">
          Connecting takes a couple of minutes on Google's site, once. Hush opens
          each page and names the button to press.
        </p>

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

