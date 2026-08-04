import { useEffect, useState } from "react";

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
 * Progress is a real count of real messages. The total is Gmail's own estimate
 * and is labelled as an estimate, because it is often wrong by a few hundred.
 */
export default function Scan({
  status,
  onFinished,
  onSkip,
}: {
  status: Status;
  onFinished: () => void;
  onSkip: () => void;
}) {
  const [depth, setDepth] = useState<ScanDepth>("one_year");
  const [running, setRunning] = useState(status.scanning);
  const [progress, setProgress] = useState<ScanProgress | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);

  useEffect(() => {
    return api.onScanProgress((p) => {
      setProgress(p);
      if (p.finished) {
        setRunning(false);
        setStopping(false);
      }
    });
  }, []);

  async function start(incremental: boolean) {
    setProblem(null);
    setProgress(null);
    setRunning(true);
    try {
      await api.startScan(depth, incremental);
    } catch (e) {
      setRunning(false);
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

  if (running || done) {
    return (
      <div className="centre narrow">
        <div className="inner stack stack-6">
          <div className="stack stack-3">
            <h1>{done ? "Finished looking" : "Looking through your mail"}</h1>
            <p className="lede">
              {done
                ? progress?.cancelled
                  ? "Stopped early — everything found so far has been kept."
                  : "Here's what turned up."
                : "You can stop whenever you like. Nothing is lost if you do."}
            </p>
          </div>

          <div className="card stack stack-4">
            <div className="row">
              {!done && <span className="spinner" aria-hidden="true" />}
              <span className="tabular" style={{ fontSize: "1.375rem", fontWeight: 600 }}>
                {(progress?.scanned ?? 0).toLocaleString()}
              </span>
              <span className="muted">
                {progress?.total_estimate
                  ? `of about ${progress.total_estimate.toLocaleString()} messages`
                  : "messages read"}
              </span>
            </div>

            {!done && (
              <Meter
                value={progress?.scanned ?? 0}
                max={progress?.total_estimate ?? 0}
              />
            )}

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
              <button className="btn-secondary" onClick={stop} disabled={stopping}>
                {stopping ? "Stopping…" : "Stop and use what's found"}
              </button>
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
