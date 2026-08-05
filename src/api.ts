/**
 * The only way this interface reaches anything outside itself.
 *
 * There is no `fetch` anywhere in the web layer, and the content security
 * policy forbids one. Every network request, every link opened, and every byte
 * written to disk happens in Rust, behind the checks in `unsub` and `commands`.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type {
  MailtoMode,
  Outcome,
  PlannedAction,
  RunReport,
  ScanDepth,
  ScanProgress,
  Sender,
  Status,
} from "./types";

export const status = () => invoke<Status>("status");
export const markWelcomeSeen = () => invoke<void>("mark_welcome_seen");

export const saveCredentials = (clientId: string, clientSecret: string) =>
  invoke<void>("save_credentials", { clientId, clientSecret });

export const connect = (allowSend: boolean, allowDelete: boolean) =>
  invoke<Status>("connect", { allowSend, allowDelete });

export const resumeSession = () => invoke<Status>("resume_session");

export const cancelConnect = () => invoke<void>("cancel_connect");

export const cancelRun = () => invoke<void>("cancel_run");

export const disconnect = (eraseLocalData: boolean) =>
  invoke<Status>("disconnect", { eraseLocalData });

export const eraseEverything = () => invoke<Status>("erase_everything");

export const startScan = (depth: ScanDepth, incremental: boolean) =>
  invoke<void>("start_scan", { depth, incremental });

export const cancelScan = () => invoke<void>("cancel_scan");

export const listSenders = () => invoke<Sender[]>("list_senders");

export const setNeverTouch = (address: string, never: boolean) =>
  invoke<void>("set_never_touch", { address, never });

export const planUnsubscribe = (addresses: string[]) =>
  invoke<PlannedAction[]>("plan_unsubscribe", { selection: { addresses } });

export const runUnsubscribe = (addresses: string[], deleteBacklog: boolean) =>
  invoke<RunReport>("run_unsubscribe", { selection: { addresses }, deleteBacklog });

export const markManualDone = (address: string) =>
  invoke<void>("mark_manual_done", { address });

export const outcomes = () => invoke<Outcome[]>("outcomes");
export const openLink = (url: string) => invoke<void>("open_link", { url });
export const setDryRun = (on: boolean) => invoke<void>("set_dry_run", { on });
export const setMailtoMode = (mode: MailtoMode) =>
  invoke<void>("set_mailto_mode", { mode });
export const dataLocation = () => invoke<string>("data_location");

/** Subscribe to scan progress. Returns an unsubscribe function. */
export function onScanProgress(handler: (p: ScanProgress) => void) {
  const pending = listen<ScanProgress>("scan-progress", (e) => handler(e.payload));
  return () => {
    void pending.then((unlisten) => unlisten());
  };
}
