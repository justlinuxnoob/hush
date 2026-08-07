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
            Hush finds everyone who mails you in bulk, shows how much each one
            sends, and unsubscribes from the ones you pick. What it can't
            unsubscribe, it filters — so nothing is ever left for you to do.
          </p>
        </div>

        <div className="promises">
          <Promise
            title="Never deletes your mail"
            body="Old newsletters move to Trash only if you ask, and Gmail keeps those for 30 days."
          />
          <Promise
            title="Never reads what's inside"
            body="Only who sent it, the subject, and when. It holds no permission that would allow more."
          />
          <Promise
            title="Never touches receipts"
            body="Order confirmations and sign-in codes carry no unsubscribe option, so they're never offered."
          />
          <Promise
            title="Never sends your data anywhere"
            body="There is no Hush server. No telemetry, no accounts, nothing measured."
          />
        </div>

        <p className="muted small">
          Setting up means clicking through a few pages on Google's site, once —
          Hush opens each one and names the button to press. That's the price of
          there being no server in the middle.
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

function Promise({ title, body }: { title: string; body: string }) {
  return (
    <div className="promise">
      <span className="promise-title">{title}</span>
      <span className="promise-body">{body}</span>
    </div>
  );
}
