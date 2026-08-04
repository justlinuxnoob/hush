/**
 * A fake backend, for working on the interface without a Google account.
 *
 * Run `VITE_HUSH_DEMO=1 npm run dev` and the app talks to this instead of Rust.
 * It is loaded through a dynamic import behind a compile-time constant, so a
 * production build drops the whole file — there is a check for that in CI.
 *
 * The sample senders are chosen to cover the cases that are awkward to
 * reproduce on demand: a flagged bank, a shop that sends both receipts and
 * marketing, a link-only sender, a mailto-only sender, and one that is already
 * protected.
 */

import type {
  Outcome,
  PlannedAction,
  RunReport,
  Sender,
  Status,
  UnsubMethod,
} from "./types";

const DAY = 86_400_000;
const now = Date.now();

let status: Status = {
  connected: true,
  email: "you@example.com",
  has_credentials: true,
  can_send: false,
  dry_run: true,
  mailto_mode: "hand_off",
  keychain_available: true,
  token_storage: "keychain",
  seen_welcome: true,
  scan_complete: true,
  last_scan_ms: now - 2 * DAY,
  message_count: 18_442,
  sender_count: 7,
  scanning: false,
};

function sender(
  name: string,
  address: string,
  count: number,
  method: UnsubMethod,
  extra: Partial<Sender> = {}
): Sender {
  return {
    address,
    display_name: name,
    message_count: count,
    first_seen_ms: now - 400 * DAY,
    last_seen_ms: now - 2 * DAY,
    frequency: "about 3 a week",
    method,
    assessment: { caution: false, score: 0, reasons: [] },
    never_touch: false,
    outcome: null,
    sample_subjects: ["An example subject", "Another one"],
    ...extra,
  };
}

const oneClick = (u: string): UnsubMethod => ({ kind: "one_click", url: u });

let senders: Sender[] = [
  sender("Daily Deals", "news@dailydeals.example", 612, oneClick("https://dailydeals.example/u/abc")),
  sender("The Morning Brief", "brief@morningbrief.example", 388, oneClick("https://morningbrief.example/u/x"), {
    frequency: "about 5 a week",
  }),
  sender("Northwind Bank", "alerts@northwindbank.example", 210, oneClick("https://northwindbank.example/u/1"), {
    assessment: {
      caution: true,
      score: 170,
      reasons: [
        "Looks like a bank or payment service",
        "Recent messages mention things like orders, receipts or bookings",
      ],
    },
    sample_subjects: ["Your monthly statement is ready", "Payment received", "Low balance alert"],
    frequency: "about 2 a week",
  }),
  sender("Fern & Thistle", "hello@fernthistle.example", 96, { kind: "manual_link", url: "https://fernthistle.example/preferences" }, {
    frequency: "about weekly",
  }),
  sender("Rust Weekly", "list@rustweekly.example", 74, {
    kind: "mailto",
    address: "unsubscribe@rustweekly.example",
    subject: "unsubscribe",
    body: null,
  }, { frequency: "about weekly" }),
  sender("Parcel Tracker", "no-reply@parceltracker.example", 51, oneClick("https://parceltracker.example/u/9"), {
    assessment: {
      caution: true,
      score: 80,
      reasons: ["Looks like a delivery or shipping service"],
    },
    sample_subjects: ["Your parcel is out for delivery", "Tracking update"],
    frequency: "about monthly",
  }),
  sender("Old Gym Membership", "news@oldgym.example", 9, oneClick("https://oldgym.example/u/2"), {
    never_touch: true,
    frequency: "a few times a year",
  }),
];

function plan(addresses: string[]): PlannedAction[] {
  return senders
    .filter((s) => addresses.includes(s.address))
    .map((s) => ({
      address: s.address,
      display_name: s.display_name,
      what:
        s.method.kind === "one_click"
          ? "Unsubscribe automatically"
          : s.method.kind === "mailto"
            ? "Open a ready-to-send email"
            : "You'll open this one yourself",
      detail:
        s.method.kind === "one_click"
          ? `POST ${s.method.url}\nContent-Type: application/x-www-form-urlencoded\n\nList-Unsubscribe=One-Click`
          : s.method.kind === "manual_link"
            ? `No request is sent. The link is listed for you to open: ${s.method.url}`
            : s.method.kind === "mailto"
              ? `Open your mail app addressed to ${s.method.address}`
              : "Nothing to do.",
    }));
}

function run(addresses: string[]): RunReport {
  const outcomes: Outcome[] = senders
    .filter((s) => addresses.includes(s.address))
    .map((s) => ({
      address: s.address,
      display_name: s.display_name,
      status: status.dry_run
        ? ("simulated" as const)
        : s.method.kind === "one_click"
          ? ("done" as const)
          : ("needs_you" as const),
      detail: status.dry_run
        ? "Dry run — nothing was sent. Would have: unsubscribe automatically"
        : s.method.kind === "one_click"
          ? "Unsubscribed automatically"
          : "Open this one yourself to finish",
      link: s.method.kind === "manual_link" ? s.method.url : null,
      at_ms: Date.now(),
    }));
  return { outcomes, handoffs: [] };
}

type Args = Record<string, unknown>;

const handlers: Record<string, (a: Args) => unknown> = {
  status: () => status,
  resume_session: () => status,
  mark_welcome_seen: () => undefined,
  save_credentials: () => undefined,
  connect: () => status,
  disconnect: () => status,
  erase_everything: () => status,
  list_senders: () => senders,
  never_touch_list: () => senders.filter((s) => s.never_touch).map((s) => s.address),
  outcomes: () => [],
  data_location: () => "~/.local/share/dev.hush.desktop/hush.sqlite3 (demo)",
  cancel_scan: () => undefined,
  open_link: (a) => {
    console.info("[demo] would open", a.url);
    return undefined;
  },
  mark_manual_done: () => undefined,
  set_dry_run: (a) => {
    status = { ...status, dry_run: Boolean(a.on) };
    return undefined;
  },
  set_mailto_mode: (a) => {
    status = { ...status, mailto_mode: a.mode as Status["mailto_mode"] };
    return undefined;
  },
  set_never_touch: (a) => {
    senders = senders.map((s) =>
      s.address === a.address ? { ...s, never_touch: Boolean(a.never) } : s
    );
    return undefined;
  },
  plan_unsubscribe: (a) => plan((a.selection as { addresses: string[] }).addresses),
  run_unsubscribe: (a) => run((a.selection as { addresses: string[] }).addresses),
  start_scan: () => {
    // Pretend to walk a mailbox so the progress screen can be looked at.
    let scanned = 0;
    const total = 18_442;
    const tick = window.setInterval(() => {
      scanned = Math.min(total, scanned + 900);
      const finished = scanned >= total;
      if (finished) window.clearInterval(tick);
      emit("scan-progress", {
        scanned,
        total_estimate: total,
        senders_found: finished ? senders.length : 0,
        finished,
        cancelled: false,
        note: null,
      });
    }, 220);
    return undefined;
  },
};

const listeners = new Map<string, ((payload: unknown) => void)[]>();

function emit(event: string, payload: unknown) {
  for (const fn of listeners.get(event) ?? []) fn(payload);
}

/** Point `@tauri-apps/api` at the fake backend. */
export function installMockBackend() {
  const internals = {
    invoke: async (cmd: string, args: Args = {}) => {
      const handler = handlers[cmd];
      if (!handler) throw { code: "other", message: `[demo] no such command: ${cmd}` };
      // A touch of latency, so loading states are visible rather than theoretical.
      await new Promise((r) => setTimeout(r, 90));
      return handler(args);
    },
    transformCallback: (cb: (v: unknown) => void) => {
      const id = Math.floor(Math.random() * 1e9);
      (window as unknown as Record<string, unknown>)[`_${id}`] = cb;
      return id;
    },
  };

  // Tauri's event plugin goes through `invoke` too; intercepting the two
  // commands it uses is enough to make `listen` work.
  handlers["plugin:event|listen"] = (a) => {
    const event = a.event as string;
    const handlerId = a.handler as number;
    const cb = (window as unknown as Record<string, (v: unknown) => void>)[`_${handlerId}`];
    const list = listeners.get(event) ?? [];
    list.push((payload) => cb?.({ event, id: handlerId, payload }));
    listeners.set(event, list);
    return handlerId;
  };
  handlers["plugin:event|unlisten"] = () => undefined;

  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = internals;
  console.info("[demo] running against the mock backend — no Google, no Rust");
}
