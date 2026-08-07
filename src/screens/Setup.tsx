import { useState } from "react";

import * as api from "../api";
import { CopyField, Notice, Steps } from "../components/ui";
import { errorMessage } from "../types";

/**
 * The Google Cloud walkthrough.
 *
 * This is where non-technical people give up, so: one step per screen, one
 * instruction per line, a button that opens the exact page, and a copy button
 * for anything that has to be typed. Never a wall of text.
 */

const STEPS = 6;

export default function Setup({
  onDone,
  onBack,
}: {
  onDone: () => void;
  onBack: () => void;
}) {
  const [step, setStep] = useState(0);
  const [clientId, setClientId] = useState("");
  const [clientSecret, setClientSecret] = useState("");
  const [fromFile, setFromFile] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const next = () => setStep((s) => Math.min(STEPS - 1, s + 1));
  const back = () => (step === 0 ? onBack() : setStep((s) => s - 1));

  async function open(url: string) {
    try {
      await api.openLink(url);
    } catch (e) {
      setProblem(errorMessage(e));
    }
  }

  /**
   * Take whatever was pasted, in whatever form Google handed it over.
   *
   * Google's clients page offers a **Download JSON** button, and that file
   * holds both values. Making someone open it, find two fields among eight and
   * hand-copy each into the right box is three chances to get it wrong — and
   * the app's own copy already admitted the most common one, pasting each into
   * the other's box.
   *
   * So either box accepts the whole file. Paste it anywhere and both fill in.
   */
  function accept(value: string, fallback: (v: string) => void) {
    const creds = parseGoogleJson(value);
    if (creds) {
      setClientId(creds.id);
      setClientSecret(creds.secret);
      setFromFile(true);
      return;
    }
    setFromFile(false);
    fallback(value);
  }

  async function save() {
    setProblem(null);
    setSaving(true);
    try {
      await api.saveCredentials(clientId, clientSecret);
      onDone();
    } catch (e) {
      setProblem(errorMessage(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="centre narrow">
      <div className="inner stack stack-6">
        <div className="row">
          <Steps total={STEPS} current={step} />
          <span className="muted small">
            Step {step + 1} of {STEPS}
            {/* The reassurance belongs here, not only on the welcome screen.
                This is the intimidating part — six pages of Google's console —
                and someone who has already clicked past the welcome text has
                no reminder that it is short. */}
            {" · about two minutes in total"}
          </span>
        </div>

        {step === 0 && (
          <Panel
            title="Why you need your own key"
            body={
              <>
                <p className="lede">
                  Most apps like this run a server that reads your email on your
                  behalf. Hush doesn't have one.
                </p>
                <p className="muted">
                  Instead, you create your own connection to Google. It belongs
                  to you, it works only on this computer, and you can switch it
                  off at any time from your Google account. It also means Hush
                  never needs Google's approval to read anyone's mail — because
                  it never does.
                </p>
                <Notice tone="calm">
                  You'll be clicking around Google's website for a few minutes.
                  It's fiddly, but you only do it once.
                </Notice>
              </>
            }
          />
        )}

        {step === 1 && (
          <Panel
            title="Create a project"
            step={1}
            body={
              <>
                <p className="muted">
                  A "project" is just a folder Google keeps your settings in.
                </p>
                <Ordered
                  items={[
                    "Open the page below and sign in if asked.",
                    'Give it any name you like — "Hush" is fine.',
                    'Leave the other boxes alone and press Create.',
                  ]}
                />
                <OpenButton
                  label="Open Google's new project page"
                  url="https://console.cloud.google.com/projectcreate"
                  onOpen={open}
                />
              </>
            }
          />
        )}

        {step === 2 && (
          <Panel
            title="Turn on Gmail"
            step={2}
            body={
              <>
                <p className="muted">
                  This tells Google your project is allowed to talk to Gmail.
                </p>
                <Ordered
                  items={[
                    "Open the page below.",
                    "Check your new project is picked at the top of the page.",
                    "Press Enable and wait a moment.",
                  ]}
                />
                <OpenButton
                  label="Open the Gmail page"
                  url="https://console.cloud.google.com/apis/library/gmail.googleapis.com"
                  onOpen={open}
                />
              </>
            }
          />
        )}

        {step === 3 && (
          <Panel
            title="Fill in the basics"
            step={3}
            body={
              <>
                <p className="muted">
                  Google asks who the app is for. Since it's just for you, the
                  answers are short.
                </p>
                <Ordered
                  items={[
                    "Open the page below and press Get started.",
                    'App name: type "Hush". Support email: pick your own address.',
                    'Audience: choose External. That sounds wrong, but it\'s the right answer for a personal account.',
                    "Contact information: your own address again. Then agree and create.",
                  ]}
                />
                <OpenButton
                  label="Open the app details page"
                  url="https://console.cloud.google.com/auth/branding"
                  onOpen={open}
                />
              </>
            }
          />
        )}

        {step === 4 && (
          <Panel
            title="Add yourself as a tester"
            step={4}
            body={
              <>
                <p className="muted">
                  Leaving the app in <em>Testing</em> means Google never has to
                  review it — nobody at Google reads anything, because nobody
                  else can use it.
                </p>
                <Ordered
                  items={[
                    "Open the page below.",
                    'Check it says Testing. If it offers to publish, don\'t.',
                    "Under Test users, press Add users and type your own Gmail address.",
                    "Press Save.",
                  ]}
                />
                <OpenButton
                  label="Open the audience page"
                  url="https://console.cloud.google.com/auth/audience"
                  onOpen={open}
                />
                <Notice tone="caution">
                  One consequence worth knowing: while the app is in Testing,
                  Google ends the connection every seven days. When that
                  happens, Hush asks you to reconnect and it's a single click.
                </Notice>
              </>
            }
          />
        )}

        {step === 5 && (
          <Panel
            title="Create the key and paste it here"
            step={5}
            body={
              <>
                <Ordered
                  items={[
                    "Open the page below and press Create client.",
                    "Application type: choose Desktop app.",
                    "Name: anything at all. Press Create.",
                    "Press Download JSON, open that file, and paste all of it into either box below — both fill in by themselves.",
                  ]}
                />
                <OpenButton
                  label="Open the clients page"
                  url="https://console.cloud.google.com/auth/clients"
                  onOpen={open}
                />

                <div className="stack stack-4" style={{ marginTop: "calc(var(--step) * 4)" }}>
                  {fromFile && (
                    <Notice tone="calm">
                      Both boxes filled in from what you pasted. Nothing else to
                      copy.
                    </Notice>
                  )}
                  <div>
                    <label htmlFor="cid">Client ID</label>
                    <input
                      id="cid"
                      type="text"
                      className="mono"
                      spellCheck={false}
                      autoComplete="off"
                      placeholder="not-a-real-example-000000.apps.googleusercontent.com"
                      value={clientId}
                      onChange={(e) => accept(e.target.value, setClientId)}
                    />
                  </div>
                  <div>
                    <label htmlFor="csec">Client secret</label>
                    <input
                      id="csec"
                      type="text"
                      className="mono"
                      spellCheck={false}
                      autoComplete="off"
                      placeholder="GOCSPX-…"
                      value={clientSecret}
                      onChange={(e) => accept(e.target.value, setClientSecret)}
                    />
                  </div>
                  <p className="muted small">
                    Shortcut: Google's clients page has a{" "}
                    <strong style={{ color: "var(--ink)" }}>Download JSON</strong>{" "}
                    button. Open that file, copy all of it, and paste it into
                    either box — both fill in by themselves.
                  </p>
                  <p className="muted small">
                    Both are stored on this computer only. Google treats
                    them as public information for desktop apps like this one —
                    they don't give anyone access to your mail on their own.
                  </p>
                </div>
              </>
            }
          />
        )}

        {problem && <Notice tone="problem">{problem}</Notice>}

        {/* Sticky: these steps walk through Google's console and run long, and
            Next should never be below the fold on a small window. */}
        <div className="decide row">
          <button className="btn-quiet" onClick={back}>
            Back
          </button>
          <div className="spacer" />
          {step < STEPS - 1 ? (
            <button className="btn-primary" onClick={next}>
              Next
            </button>
          ) : (
            <button
              className="btn-primary"
              onClick={save}
              disabled={saving || !clientId.trim() || !clientSecret.trim()}
            >
              {saving ? "Checking…" : "Save and continue"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * Pull the two values out of Google's downloaded client file.
 *
 * The shape is `{"installed": {...}}` for a Desktop app and `{"web": {...}}`
 * for the other type — accepting both means someone who picked the wrong client
 * type gets a real error from Google later rather than a confusing one here.
 * Anything that is not that file returns null and is treated as ordinary typing.
 */
export function parseGoogleJson(text: string): { id: string; secret: string } | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("{")) return null;
  try {
    const parsed = JSON.parse(trimmed);
    const inner = parsed.installed ?? parsed.web ?? parsed;
    const id = typeof inner.client_id === "string" ? inner.client_id.trim() : "";
    const secret =
      typeof inner.client_secret === "string" ? inner.client_secret.trim() : "";
    return id && secret ? { id, secret } : null;
  } catch {
    return null;
  }
}

/**
 * Screenshots of Google's own pages, keyed by step number.
 *
 * Loaded by filename rather than imported one by one, so adding a picture is
 * dropping a file into `src/assets/setup/` and nothing else. A step with no
 * file shows no picture, which is why this cannot break a build — the pages
 * these show are Google's and they get redesigned without warning, so a
 * missing or stale image must always degrade to the words alone.
 */
const SHOTS = import.meta.glob("../assets/setup/step-*.png", {
  eager: true,
  query: "?url",
  import: "default",
}) as Record<string, string>;

/**
 * Every picture for a step, in filename order.
 *
 * A step is not always one action. The last one is three — create the client,
 * choose Desktop app, download the file — so `step-5.png`, `step-5b.png` and
 * `step-5c.png` all belong to it and all show, in that order.
 */
function shotsFor(step: number): string[] {
  return Object.entries(SHOTS)
    .filter(([path]) => /\/step-(\d+)[a-z]?\.png$/.test(path) && path.includes(`step-${step}`))
    .filter(([path]) => new RegExp(`/step-${step}[a-z]?\\.png$`).test(path))
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([, url]) => url);
}

function Panel({
  title,
  body,
  step,
}: {
  title: string;
  body: React.ReactNode;
  /** 1-based, matching the screenshot filenames. Omit for steps with none. */
  step?: number;
}) {
  const shots = step ? shotsFor(step) : [];
  return (
    <div className="stack stack-6">
      <h1 style={{ fontSize: "1.625rem" }}>{title}</h1>
      <div className="stack stack-4">{body}</div>
      {shots.length > 0 && (
        <figure className="shot">
          {shots.map((src, i) => (
            <img key={src} src={src} alt={`Step ${step}, part ${i + 1}, on Google's site`} />
          ))}
          <figcaption className="muted small">
            What you're looking for. Google redesigns these pages from time to
            time, so yours may not match exactly — the words above are what
            matter.
          </figcaption>
        </figure>
      )}
    </div>
  );
}

function Ordered({ items }: { items: string[] }) {
  return (
    <ol className="stack stack-2" style={{ paddingLeft: "1.2rem", margin: 0 }}>
      {items.map((t) => (
        <li key={t} className="muted">
          {t}
        </li>
      ))}
    </ol>
  );
}

/**
 * Opens a page in the real browser, and shows the address alongside.
 *
 * The address is visible and copyable on purpose: a button that goes somewhere
 * unnamed is exactly the pattern people are told not to trust.
 */
/**
 * Google's console silently picks an account for you.
 *
 * Anyone signed into more than one Google account — most people — gets the
 * console on whichever it saw first, not the one they mean to use. The whole
 * setup then completes against the wrong account and the failure surfaces much
 * later as "Google turned the request down", with nothing pointing at why.
 *
 * Said on every page that opens the console rather than once at the start.
 * Repetition is right when the mistake is invisible, easy, and expensive.
 */
function WrongAccountWarning() {
  return (
    <p className="muted small">
      <strong style={{ color: "var(--caution)" }}>Check the account.</strong>{" "}
      Google picks one for you and it is often not the one you want. Look at the
      circle in the top right of the page and switch to the Gmail account you
      are tidying up — check it on every page, because it can change back.
    </p>
  );
}

function OpenButton({
  label,
  url,
  onOpen,
}: {
  label: string;
  url: string;
  onOpen: (url: string) => void;
}) {
  const [showing, setShowing] = useState(false);
  return (
    <div className="stack stack-2">
      <div className="row row-tight">
        <button className="btn-secondary" onClick={() => onOpen(url)}>
          {label}
        </button>
        <button className="btn-quiet btn-small" onClick={() => setShowing((s) => !s)}>
          {showing ? "Hide address" : "Show address"}
        </button>
      </div>
      {showing && <CopyField label="It opens this page" value={url} />}
      {/* Attached to the button rather than written into each step, so a step
          added later cannot forget it. */}
      <WrongAccountWarning />
    </div>
  );
}
