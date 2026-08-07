import { useCallback, useEffect, useState } from "react";

import * as api from "../api";
import { Notice, plural } from "../components/ui";
import {
  errorMessage,
  type ManagedFilter,
  type RemovalPreview,
  type RemovalReport,
  type Status,
} from "../types";

/**
 * Undoing a block, without going to Gmail's settings to do it.
 *
 * Everything on this screen is read live from the account. Hush keeps no list
 * of what it blocked — Gmail already has one, and a second copy could only ever
 * disagree with the first. It also means this screen is correct on a machine
 * that has never seen the account before.
 *
 * Filters Hush did not create are shown and never touched. They belong to
 * someone who wrote them for a reason nobody here knows.
 */
export default function Blocks({
  status,
  onClose,
  onStatusChange,
}: {
  status: Status;
  onClose: () => void;
  onStatusChange: (s: Status) => void;
}) {
  const [filters, setFilters] = useState<ManagedFilter[] | null>(null);
  const [query, setQuery] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);

  const [removing, setRemoving] = useState<ManagedFilter | null>(null);
  const [preview, setPreview] = useState<RemovalPreview | null>(null);
  const [restore, setRestore] = useState(true);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<RemovalReport | null>(null);

  const load = useCallback(() => {
    if (!status.can_block) return;
    setProblem(null);
    api
      .listBlocks()
      .then(setFilters)
      .catch((e) => {
        setFilters([]);
        setProblem(errorMessage(e));
      });
  }, [status.can_block]);

  useEffect(load, [load]);

  /** The permission for filters covers reading them as well as making them. */
  async function askForFilters() {
    setProblem(null);
    setAsking(true);
    try {
      onStatusChange(await api.connect(status.can_delete, true));
    } catch (e) {
      setProblem(errorMessage(e));
    } finally {
      setAsking(false);
    }
  }

  async function startRemoval(f: ManagedFilter) {
    setRemoving(f);
    setPreview(null);
    setDone(null);
    setRestore(true);
    try {
      setPreview(await api.previewBlockRemoval(f.id));
    } catch (e) {
      // The count is a nicety. Not being able to work it out is no reason to
      // stand between someone and undoing something they regret.
      setProblem(errorMessage(e));
    }
  }

  async function confirmRemoval() {
    if (!removing) return;
    setBusy(true);
    setProblem(null);
    try {
      const report = await api.removeBlock(removing.id, restore && status.can_delete);
      setDone(report);
      setRemoving(null);
      load();
    } catch (e) {
      setProblem(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  if (!status.can_block) {
    return (
      <Shell onClose={onClose}>
        <Notice tone="calm">
          Your blocks live in your Gmail settings, and Hush needs Google's
          permission for filters before it can read them back. Nothing is stored
          here, so there's nothing to see until it can ask.
        </Notice>
        <div>
          <button className="btn-primary" onClick={askForFilters} disabled={asking}>
            {asking ? "Waiting for your browser…" : "Let Hush read your filters"}
          </button>
        </div>
      </Shell>
    );
  }

  // Matches the address and what the filter does, so "trash" finds every
  // block set to delete and a domain finds every address under it.
  const q = query.trim().toLowerCase();
  const matches = (f: ManagedFilter) =>
    !q || `${f.address} ${f.summary}`.toLowerCase().includes(q);

  const all = filters ?? [];
  const mine = all.filter((f) => f.mine && matches(f));
  const theirs = all.filter((f) => !f.mine && matches(f));
  const hidden = all.length - mine.length - theirs.length;

  return (
    <Shell onClose={onClose}>
      {problem && <Notice tone="problem">{problem}</Notice>}

      {done && (
        <Notice tone="calm">
          <div className="stack stack-2">
            <span>
              <strong>Block removed.</strong> Their mail will reach your inbox
              again from now on.
            </span>
            {done.restored > 0 && (
              <span>
                {plural(done.restored, "old email")} put back in your inbox.
              </span>
            )}
            {done.restore_failed > 0 && (
              <span>
                {plural(done.restore_failed, "email")} couldn't be moved back.
                They're still in your account — search for the sender to find
                them.
              </span>
            )}
            {done.problem && <span className="muted small">{done.problem}</span>}
          </div>
        </Notice>
      )}

      {filters !== null && filters.length > 0 && (
        <div className="search">
          <input
            type="search"
            placeholder="Search blocked senders"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            aria-label="Search blocked senders"
          />
        </div>
      )}

      {filters === null ? (
        <div className="row">
          <span className="spinner" aria-hidden="true" />
          <span className="muted">Asking Gmail what's blocked…</span>
        </div>
      ) : mine.length === 0 && theirs.length === 0 && hidden > 0 ? (
        <div className="panel stack stack-2">
          <h3>Nothing matches "{query}"</h3>
          <p className="muted">
            {plural(hidden, "filter")} on this account, none with that in the
            address or the action.
          </p>
          <div>
            <button className="btn-secondary" onClick={() => setQuery("")}>
              Clear the search
            </button>
          </div>
        </div>
      ) : mine.length === 0 && theirs.length === 0 ? (
        <div className="panel stack stack-2">
          <h3>Nothing is blocked yet</h3>
          <p className="muted">
            When you block a sender, the rule appears here — and in your Gmail
            settings, because that's where it actually lives. You can undo any of
            them from this screen.
          </p>
        </div>
      ) : (
        <div className="stack stack-6">
          {mine.length > 0 && (
            <div className="stack stack-3">
              <h3>Blocked by Hush</h3>
              <div className="stack stack-2">
                {mine.map((f) => (
                  <div key={f.id} className="result-row">
                    <div className="stack" style={{ minWidth: 0 }}>
                      <span className="result-name">{f.address || "Any sender"}</span>
                      <span className="muted small">{f.summary}</span>
                    </div>
                    <div className="spacer" />
                    <span
                      className={`badge ${f.action === "trash" ? "badge-caution" : "badge-neutral"}`}
                    >
                      {f.action === "trash" ? "To Trash" : "Archived"}
                    </span>
                    <button
                      className="btn-secondary btn-small"
                      onClick={() => startRemoval(f)}
                      disabled={busy}
                    >
                      Unblock
                    </button>
                  </div>
                ))}
              </div>
            </div>
          )}

          {theirs.length > 0 && (
            <div className="stack stack-3">
              <h3>Your own filters</h3>
              <span className="muted small">
                Hush didn't create these, so it won't change or remove them.
                They're here so the list is the whole truth about your account.
              </span>
              <div className="stack stack-2">
                {theirs.map((f) => (
                  <div key={f.id} className="result-row" style={{ opacity: 0.75 }}>
                    <div className="stack" style={{ minWidth: 0 }}>
                      <span className="result-name">{f.address || "Any sender"}</span>
                      <span className="muted small">{f.summary}</span>
                    </div>
                    <div className="spacer" />
                    <span className="badge badge-neutral">Yours</span>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      )}

      {removing && (
        <div className="card stack stack-4">
          <div className="stack">
            <h3>Unblock {removing.address}?</h3>
            <span className="muted small">
              Their mail reaches your inbox normally again. The filter is deleted
              from your Gmail settings.
            </span>
          </div>

          {preview === null ? (
            <div className="row">
              <span className="spinner" aria-hidden="true" />
              <span className="muted small">Counting what it caught…</span>
            </div>
          ) : (
            <>
              {preview.in_trash + preview.archived > 0 ? (
                <label
                  className="row"
                  style={{ marginBottom: 0, cursor: "pointer", alignItems: "flex-start" }}
                >
                  <input
                    type="checkbox"
                    checked={restore && status.can_delete}
                    disabled={!status.can_delete}
                    onChange={(e) => setRestore(e.target.checked)}
                    style={{ marginTop: "5px", accentColor: "var(--accent)" }}
                  />
                  <span>
                    <strong>
                      Put back the{" "}
                      {plural(preview.in_trash + preview.archived, "email")} this
                      block caught
                      {preview.approximate && " so far"}
                    </strong>
                    <span className="muted small" style={{ display: "block", fontWeight: 400 }}>
                      {status.can_delete
                        ? "They go back to your inbox. Only mail this block moved — anything you filed yourself stays put."
                        : "Needs Google's permission to manage your mail, which Hush doesn't have. The block still comes off."}
                    </span>
                  </span>
                </label>
              ) : (
                <span className="muted small">
                  Nothing of theirs is waiting to be put back.
                </span>
              )}

              {preview.action === "trash" && (
                <Notice tone="caution">
                  This one sent their mail to Trash. Anything Gmail already
                  deleted — Trash empties after 30 days — is gone for good, and
                  nothing here can bring it back.
                </Notice>
              )}
            </>
          )}

          <div className="row">
            <button className="btn-quiet" onClick={() => setRemoving(null)} disabled={busy}>
              Keep the block
            </button>
            <div className="spacer" />
            <button className="btn-primary" onClick={confirmRemoval} disabled={busy}>
              {busy ? "Working…" : "Unblock"}
            </button>
          </div>
        </div>
      )}
    </Shell>
  );
}

function Shell({ children, onClose }: { children: React.ReactNode; onClose: () => void }) {
  return (
    <div className="centre">
      <div className="inner stack stack-6">
        <div className="escape row">
          <h1>Blocked senders</h1>
          <div className="spacer" />
          <button className="btn-quiet" onClick={onClose}>
            Close
          </button>
        </div>
        <span className="muted small">
          Read from your Gmail settings every time you open this. Hush keeps no
          list of its own, so this is always what your account actually says.
        </span>
        {children}
      </div>
    </div>
  );
}
