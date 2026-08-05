/** Mirrors of the Rust types that cross the bridge. */

export type UnsubMethod =
  | { kind: "one_click"; url: string }
  | { kind: "mailto"; address: string; subject: string | null; body: string | null }
  | { kind: "manual_link"; url: string }
  | { kind: "none" };

export interface Assessment {
  caution: boolean;
  score: number;
  reasons: string[];
}

export type OutcomeStatus = "done" | "sent" | "needs_you" | "failed";

export interface Outcome {
  address: string;
  display_name: string;
  status: OutcomeStatus;
  detail: string;
  link: string | null;
  at_ms: number;
}

export interface Sender {
  address: string;
  display_name: string;
  message_count: number;
  /** How many of those carried an unsubscribe header — the ones a tidy-up would bin. */
  bulk_count: number;
  first_seen_ms: number;
  last_seen_ms: number;
  frequency: string;
  method: UnsubMethod;
  assessment: Assessment;
  never_touch: boolean;
  outcome: Outcome | null;
  sample_subjects: string[];
}

export type MailtoMode = "hand_off" | "send_via_gmail";
export type TokenStorage = "keychain" | "memory";
export type ScanDepth = "six_months" | "one_year" | "two_years" | "everything";

export interface Status {
  connected: boolean;
  email: string | null;
  has_credentials: boolean;
  can_send: boolean;
  can_delete: boolean;
  can_block: boolean;
  mailto_mode: MailtoMode;
  keychain_available: boolean;
  token_storage: TokenStorage | null;
  seen_welcome: boolean;
  scan_complete: boolean;
  last_scan_ms: number;
  message_count: number;
  sender_count: number;
  scanning: boolean;
}

export interface ScanProgress {
  scanned: number;
  /** Exact count of messages in scope. 0 while still counting. */
  total: number;
  /** True during the counting pass, before anything has been read. */
  counting: boolean;
  /** How many have been found so far, while counting. */
  found: number;
  senders_found: number;
  finished: boolean;
  cancelled: boolean;
  note: string | null;
}

/** Progress while unsubscribes and binning are running. */
export interface RunProgress {
  doing: string;
  done: number;
  total: number;
  binning: boolean;
  finished: boolean;
}

export interface PlannedAction {
  address: string;
  display_name: string;
  what: string;
  detail: string;
}

export interface TrashReport {
  trashed: number;
  failed: number;
  /** How many binned messages Gmail still showed afterwards. null = not checked. */
  still_present: number | null;
  /** Why the first failure failed, if anything failed. */
  problem: string | null;
}

export interface SenderMessage {
  subject: string;
  date_ms: number;
}

export interface BlockReport {
  blocked: number;
  failed: number;
  problem: string | null;
}

export interface RunReport {
  outcomes: Outcome[];
  handoffs: { address: string; mailto_url: string }[];
  trash: TrashReport | null;
  blocked: BlockReport | null;
}

/** The shape every rejected command takes. */
export interface AppError {
  code: string;
  message: string;
}

/** Errors arrive as structured values, but a thrown string is still possible. */
export function errorMessage(e: unknown): string {
  if (typeof e === "object" && e !== null && "message" in e) {
    return String((e as AppError).message);
  }
  if (typeof e === "string") return e;
  return "Something went wrong. Please try again.";
}

export function errorCode(e: unknown): string {
  if (typeof e === "object" && e !== null && "code" in e) {
    return String((e as AppError).code);
  }
  return "other";
}

/** How a sender's unsubscribe will be carried out, in plain words. */
export function methodLabel(m: UnsubMethod): string {
  switch (m.kind) {
    case "one_click":
      return "Automatic";
    case "mailto":
      return "By email";
    case "manual_link":
      return "One click from you";
    default:
      return "No option";
  }
}
