import { useCallback, useEffect, useState } from "react";

import * as api from "./api";
import { Notice, Wordmark } from "./components/ui";
import Confirm from "./screens/Confirm";
import Connect from "./screens/Connect";
import Results from "./screens/Results";
import Scan from "./screens/Scan";
import SenderList from "./screens/SenderList";
import Settings from "./screens/Settings";
import Setup from "./screens/Setup";
import Welcome from "./screens/Welcome";
import { errorMessage, type RunReport, type Sender, type Status } from "./types";

type Screen =
  | "loading"
  | "welcome"
  | "setup"
  | "connect"
  | "scan"
  | "list"
  | "confirm"
  | "results"
  | "settings";

export default function App() {
  const [screen, setScreen] = useState<Screen>("loading");
  const [status, setStatus] = useState<Status | null>(null);
  const [senders, setSenders] = useState<Sender[]>([]);
  const [chosen, setChosen] = useState<string[]>([]);
  const [report, setReport] = useState<RunReport | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  const reloadSenders = useCallback(async () => {
    try {
      setSenders(await api.listSenders());
    } catch {
      // Not being connected is the usual reason, and the screen already says so.
      setSenders([]);
    }
  }, []);

  /** Pick the screen that matches where the user actually is. */
  const route = useCallback(
    (s: Status) => {
      if (!s.seen_welcome) return setScreen("welcome");
      if (!s.has_credentials) return setScreen("setup");
      if (!s.connected) return setScreen("connect");
      if (!s.scan_complete && s.message_count === 0) return setScreen("scan");
      return setScreen("list");
    },
    []
  );

  useEffect(() => {
    (async () => {
      try {
        const s = await api.resumeSession();
        setStatus(s);
        await reloadSenders();
        route(s);
      } catch (e) {
        setProblem(errorMessage(e));
        setScreen("welcome");
      }
    })();
  }, [reloadSenders, route]);

  async function refresh() {
    const s = await api.status();
    setStatus(s);
    return s;
  }

  if (screen === "loading" || !status) {
    return (
      <div className="app">
        <div className="centre">
          <div className="inner stack stack-4">
            <Wordmark />
            {problem ? <Notice tone="problem">{problem}</Notice> : <span className="muted">One moment…</span>}
          </div>
        </div>
      </div>
    );
  }

  // The first-run screens are deliberately chrome-free: nothing to click but
  // the thing being asked for.
  const bare = screen === "welcome" || screen === "setup" || screen === "connect";

  return (
    <div className="app">
      {!bare && (
        <header className="topbar">
          <Wordmark />
          <div className="spacer" />
          {status.email && <span className="muted small">{status.email}</span>}
          {!status.connected && screen !== "settings" && (
            <button className="btn-secondary btn-small" onClick={() => setScreen("connect")}>
              Reconnect
            </button>
          )}
          <button
            className="btn-quiet btn-small"
            onClick={() => setScreen(screen === "settings" ? "list" : "settings")}
          >
            {screen === "settings" ? "Close" : "Settings"}
          </button>
        </header>
      )}

      {screen === "welcome" && (
        <Welcome
          onNext={async () => {
            await api.markWelcomeSeen();
            const s = await refresh();
            setScreen(s.has_credentials ? "connect" : "setup");
          }}
        />
      )}

      {screen === "setup" && (
        <Setup
          onBack={() => setScreen("welcome")}
          onDone={async () => {
            await refresh();
            setScreen("connect");
          }}
        />
      )}

      {screen === "connect" && (
        <Connect
          status={status}
          onBack={() => setScreen("setup")}
          onConnected={async (s) => {
            setStatus(s);
            await reloadSenders();
            setScreen(s.scan_complete ? "list" : "scan");
          }}
        />
      )}

      {screen === "scan" && (
        <Scan
          status={status}
          onSkip={() => setScreen("list")}
          onFinished={async () => {
            await refresh();
            await reloadSenders();
            setScreen("list");
          }}
        />
      )}

      {screen === "list" && (
        <SenderList
          senders={senders}
          onReload={reloadSenders}
          onRescan={() => setScreen("scan")}
          onContinue={(addresses) => {
            setChosen(addresses);
            setScreen("confirm");
          }}
        />
      )}

      {screen === "confirm" && (
        <Confirm
          status={status}
          senders={senders}
          addresses={chosen}
          onStatusChange={setStatus}
          onBack={() => setScreen("list")}
          onDone={async (r) => {
            setReport(r);
            await reloadSenders();
            setScreen("results");
          }}
        />
      )}

      {screen === "results" && report && (
        <Results
          status={status}
          report={report}
          onFinish={async () => {
            await reloadSenders();
            setChosen([]);
            setReport(null);
            setScreen("list");
          }}
          onDoItForReal={async () => {
            await api.setDryRun(false);
            setStatus(await api.status());
            setReport(null);
            // Straight back to the confirmation with the same senders picked,
            // so "do it for real" is one more click rather than a re-run of
            // the whole selection.
            setScreen("confirm");
          }}
        />
      )}

      {screen === "settings" && (
        <Settings
          status={status}
          onStatus={setStatus}
          onClose={() => setScreen("list")}
          onReset={async () => {
            setSenders([]);
            setChosen([]);
            setReport(null);
            const s = await refresh();
            route(s);
          }}
        />
      )}
    </div>
  );
}
