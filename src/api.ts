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
  BacklogAction,
  BlockAction,
  Check,
  MailtoMode,
  ManagedFilter,
  Outcome,
  PlannedAction,
  RemovalPreview,
  RemovalReport,
  RunReport,
  ScanDepth,
  RunProgress,
  ScanProgress,
  Sender,
  SenderMessage,
  Status,
} from "./types";

export const status = () => invoke<Status>("status");
export const markWelcomeSeen = () => invoke<void>("mark_welcome_seen");

export const saveCredentials = (clientId: string, clientSecret: string) =>
  invoke<void>("save_credentials", { clientId, clientSecret });

export const connect = (
  allowSend: boolean,
  allowDelete: boolean,
  allowBlock: boolean
) => invoke<Status>("connect", { allowSend, allowDelete, allowBlock });

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

export const senderMessages = (address: string) =>
  invoke<SenderMessage[]>("sender_messages", { address });

export const setNeverTouch = (address: string, never: boolean) =>
  invoke<void>("set_never_touch", { address, never });

export const planUnsubscribe = (addresses: string[]) =>
  invoke<PlannedAction[]>("plan_unsubscribe", { selection: { addresses } });

export const runUnsubscribe = (
  addresses: string[],
  unsubscribe: boolean,
  deleteBacklog: boolean,
  blockFuture: boolean,
  blockAction: BlockAction,
  backlogAction: BacklogAction
) =>
  invoke<RunReport>("run_unsubscribe", {
    selection: { addresses },
    unsubscribe,
    deleteBacklog,
    blockFuture,
    blockAction,
    backlogAction,
  });

/**
 * Check everything against Google, not against what Hush has cached.
 *
 * The one thing the app cannot otherwise notice: permissions revoked from a
 * Google account page. Nothing tells it, so it keeps believing until an
 * operation fails.
 */
export const diagnose = () => invoke<Check[]>("diagnose");

/**
 * The account's filters, read live from Gmail every time.
 *
 * Hush stores no list of what it blocked, so there is nothing here to go stale
 * and nothing to migrate between machines.
 */
export const listBlocks = () => invoke<ManagedFilter[]>("list_blocks");
export const previewBlockRemoval = (id: string) =>
  invoke<RemovalPreview>("preview_block_removal", { id });
export const removeBlock = (id: string, restore: boolean) =>
  invoke<RemovalReport>("remove_block", { id, restore });

export const outcomes = () => invoke<Outcome[]>("outcomes");
export const openLink = (url: string) => invoke<void>("open_link", { url });
export const setMailtoMode = (mode: MailtoMode) =>
  invoke<void>("set_mailto_mode", { mode });
export const dataLocation = () => invoke<string>("data_location");
export const openDataFolder = () => invoke<void>("open_data_folder");

/** Follow a run in progress. Returns an unsubscribe function. */
export function onRunProgress(handler: (p: RunProgress) => void) {
  const pending = listen<RunProgress>("run-progress", (e) => handler(e.payload));
  return () => {
    void pending.then((unlisten) => unlisten());
  };
}

/** Subscribe to scan progress. Returns an unsubscribe function. */
export function onScanProgress(handler: (p: ScanProgress) => void) {
  const pending = listen<ScanProgress>("scan-progress", (e) => handler(e.payload));
  return () => {
    void pending.then((unlisten) => unlisten());
  };
}
