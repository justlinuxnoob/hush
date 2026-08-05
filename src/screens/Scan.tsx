import { useState } from "react";

import * as api from "../api";
import { Meter, Notice, plural } from "../components/ui";
import { errorMessage, type ScanDepth, type ScanProgress, type Status } from "../types";

const DEPTHS: { value: ScanDepth; title: string; why: string }[] = [
  {
    value: "six_months",
    title: "The last 6 months",
    why: "Fastest. Catches almost everything that still mails you.",
  },
  {
    value: "one_year",
    title: "The last year",
    why: "A good balance for most inboxes.",
  },
  {
    value: "two_years",
    title: "The last 2 years",
    why: "Finds the occasional sender too.",
  },
  {
    value: "everything",
    title: "Everything",
    why: "Thorough, and slow on a large mailbox. You can stop at any point.",
  },
];

/**
 * Choosing how far back to look, and then watching it happen.
 *
 * Progress is a real count of real messages. The total beside it is Gmail's own
 * estimate, labelled as one, and dropped entirely once the real count overtakes
 * it — that estimate can be out by orders of magnitude in either direction.
 */
export default function Scan({
  status,
  progress,
  onProgress,
  onFinished,
  onSkip,
}: {
  status: Status;
  /** Owned by the app, so leaving this screen does not lose a running scan. */
  progress: ScanProgress | null;
  onProgress: (p: ScanProgress | null) => void;
  onFinished: () => void;
  onSkip: () => void;
}) {
  const [depth, setDepth] = useState<ScanDepth>("one_year");
  const [problem, setProblem] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);

  // A scan is running if the backend says so, or if progress has arrived and
  // has not reported itself finished. Deriving it beats a local flag that goes
  // stale the moment this screen is unmounted and remounted.
  const running = (status.scanning || progress !== null) && !(progress?.finished ?? false);

  async function start(incremental: boolean) {
    setProblem(null);
    onProgress(null);
    try {
      await api.startScan(depth, incremental);
    } catch (e) {
      setProblem(errorMessage(e));
    }
  }

  async function stop() {
    setStopping(true);
    try {
      await api.cancelScan();
    } catch (e) {
      setProblem(errorMessage(e));
    }
  }

  const done = progress?.finished ?? false;
  const scanned = progress?.scanned ?? 0;
  const total = progress?.total ?? 0;
  const counting = (progress?.counting ?? false) && !done;

  if (running || done) {
    return (
      <div className="centre narrow">
        <div className="inner stack stack-6">
          <div className="stack stack-3">
            <h1>
              {done
                ? "Finished looking"
                : counting
                  ? "Counting your emails"
                  : "Reading your emails"}
            </h1>
            <p className="lede">
              {done
                ? progress?.cancelled
                  ? "Stopped early — everything found so far has been kept."
                  : "Here's what turned up."
                : counting
                  ? "Finding out exactly how many there are, so the next bit can tell you the truth about how far along it is."
                  : "You can stop whenever you like. Nothing is lost if you do."}
            </p>
          </div>

          <div className="card stack stack-4">
            <div className="row">
              {!done && <span className="spinner" aria-hidden="true" />}
              <span className="tabular" style={{ fontSize: "1.375rem", fontWeight: 600 }}>
                {counting
                  ? (progress?.found ?? 0).toLocaleString()
                  : scanned.toLocaleString()}
              </span>
              <span className="muted">
                {counting
                  ? "found so far"
                  : total > 0
                    ? `of ${total.toLocaleString()} messages`
                    : "messages read"}
              </span>
            </div>

            {!done && <Meter value={scanned} max={counting ? 0 : total} />}

            {done && (
              <p className="muted">
                Found {plural(progress?.senders_found ?? 0, "sender")} you can
                unsubscribe from.
              </p>
            )}
          </div>

          {progress?.note && <Notice tone="caution">{progress.note}</Notice>}
          {problem && <Notice tone="problem">{problem}</Notice>}

          <div className="row">
            {done ? (
              <>
                <button className="btn-primary" onClick={onFinished} autoFocus>
                  See who's mailing you
                </button>
                {progress?.cancelled && (
                  <button className="btn-quiet" onClick={() => start(false)}>
                    Carry on scanning
                  </button>
                )}
              </>
            ) : (
              <div className="stack stack-2">
                <div>
                  <button className="btn-secondary" onClick={stop} disabled={stopping}>
                    {stopping ? "Stopping…" : "Stop and use what's found"}
                  </button>
                </div>
                {stopping && (
                  <span className="muted small">
                    Finishing the requests already in flight — a moment.
                  </span>
                )}
              </div>
            )}
          </div>
        </div>
      </div>
    );
  }

  const hasPrevious = status.scan_complete && status.message_count > 0;

  return (
    <div className="centre narrow">
      <div className="inner stack stack-6">
        <div className="stack stack-3">
          <h1>How far back should we look?</h1>
          <p className="lede">
            Hush reads who sent each message and its subject line. It never asks
            for what's inside, and it changes nothing.
          </p>
        </div>

        {hasPrevious && (
          <div className="card stack stack-3">
            <h3>Pick up where you left off</h3>
            <p className="muted small">
              Hush already knows about{" "}
              {plural(status.message_count, "message")} from last time. It can
              ask Google for just the new ones, which takes seconds.
            </p>
            <div className="row">
              <button className="btn-primary" onClick={() => start(true)}>
                Check for new mail
              </button>
              <button className="btn-quiet" onClick={onSkip}>
                Skip — show me the list
              </button>
            </div>
          </div>
        )}

        <div className="choices">
          {DEPTHS.map((d) => (
            <button
              key={d.value}
              className="choice"
              aria-pressed={depth === d.value}
              onClick={() => setDepth(d.value)}
            >
              <span>
                <strong>{d.title}</strong>
                <span className="why">{d.why}</span>
              </span>
            </button>
          ))}
        </div>

        {problem && <Notice tone="problem">{problem}</Notice>}

        <div className="row">
          <button className="btn-primary" onClick={() => start(false)}>
            {hasPrevious ? "Start a fresh scan" : "Start looking"}
          </button>
          <span className="muted small">
            A large mailbox can take a while. You can stop at any point.
          </span>
        </div>
      </div>
    </div>
  );
}
