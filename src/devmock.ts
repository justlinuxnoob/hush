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
  BlockAction,
  ManagedFilter,
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
  block_action: "archive",
  backlog_action: "archive",
  can_send: false,
  can_delete: false,
  can_block: false,
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
    // Most bulk mail carries the header; a slice of it doesn't, standing in for
    // the receipts a shop sends from the same address.
    bulk_count: Math.max(1, Math.round(count * 0.8)),
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

/**
 * Mirror the guards in the real `resolve()`: a sender must still be in the list
 * and must not be protected. Without these the mock would be more permissive
 * than the backend, and someone testing the interface against it could conclude
 * the never-touch list does nothing.
 */
function selectable(addresses: string[]): Sender[] {
  return senders.filter((s) => addresses.includes(s.address) && !s.never_touch);
}

function plan(addresses: string[]): PlannedAction[] {
  return selectable(addresses)
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

function run(
  addresses: string[],
  unsubscribe: boolean,
  deleteBacklog: boolean,
  blockFuture: boolean,
  blockAction: BlockAction
): RunReport {
  const outcomes: Outcome[] = (unsubscribe ? selectable(addresses) : [])
    .map((s) => ({
      address: s.address,
      display_name: s.display_name,
      status:
        s.method.kind === "one_click"
          ? ("sent" as const)
          : ("could_not_automate" as const),
      detail:
        s.method.kind === "one_click"
          ? "Their server accepted it"
          : "This sender only offers an unsubscribe you'd have to click yourself",
      link: s.method.kind === "manual_link" ? s.method.url : null,
      at_ms: Date.now(),
    }));
  const binned = selectable(addresses).reduce((n, s) => n + s.bulk_count, 0);
  {
    // The real backend records outcomes, which is what puts the "Already
    // handled" badge on a sender. Mirror it so the demo matches.
    const byAddress = new Map(outcomes.map((o) => [o.address, o]));
    senders = senders.map((s) =>
      byAddress.has(s.address) ? { ...s, outcome: byAddress.get(s.address)! } : s
    );
  }

  if (deleteBacklog) {
    // The real backend forgets binned mail so counts stay true; do the same.
    const hit = new Set(selectable(addresses).map((s) => s.address));
    senders = senders
      .map((s) =>
        hit.has(s.address)
          ? { ...s, message_count: s.message_count - s.bulk_count, bulk_count: 0 }
          : s
      )
      .filter((s) => s.message_count > 0);
  }

  return {
    outcomes,
    blocked: blockFuture
      ? {
          blocked: selectable(addresses).length,
          failed: 0,
          problem: null,
          confirmed: selectable(addresses).length,
          action: blockAction,
          unmarked: false,
        }
      : null,
    trash: deleteBacklog
      ? {
          action: "archive" as const,
          trashed: binned,
          failed: 0,
          still_present: 0,
          problem: null,
        }
      : null,
  };
}

let demoFilters: ManagedFilter[] = [
  {
    id: "f1",
    address: "news@dailydeals.example",
    summary: "Keeps their mail out of the inbox. Nothing is deleted",
    action: "archive",
    mine: true,
  },
  {
    id: "f2",
    address: "offers@megastore.example",
    summary: "Moves their mail to Trash, where Gmail deletes it after 30 days",
    action: "trash",
    mine: true,
  },
  {
    id: "f3",
    address: "boss@work.example",
    summary: "One of your own filters — it adds a label",
    action: null,
    mine: false,
  },
];

type Args = Record<string, unknown>;

const handlers: Record<string, (a: Args) => unknown> = {
  status: () => status,
  resume_session: () => status,
  mark_welcome_seen: () => undefined,
  save_credentials: () => undefined,
  cancel_connect: () => undefined,
  cancel_run: () => undefined,
  connect: (a) => {
    // Mirrors the real flow: Google grants what was asked for.
    status = {
      ...status,
      connected: true,
      can_send: status.can_send || Boolean(a.allowSend),
      can_delete: status.can_delete || Boolean(a.allowDelete),
      can_block: status.can_block || Boolean(a.allowBlock),
    };
    return status;
  },
  disconnect: () => status,
  erase_everything: () => status,
  list_senders: () =>
    import.meta.env.VITE_HUSH_MANY
      ? [
          ...senders,
          ...Array.from({ length: Number(import.meta.env.VITE_HUSH_MANY) }, (_, i) =>
            sender(
              `Sender Number ${i}`,
              `bulk${i}@example.com`,
              Math.max(1, 900 - i),
              oneClick(`https://example.com/u/${i}`)
            )
          ),
        ]
      : senders,
  sender_messages: (a) => {
    const s = senders.find((x) => x.address === a.address);
    const n = s?.message_count ?? 0;
    // Enough rows to make the scrolling worth looking at.
    return Array.from({ length: Math.min(n, 120) }, (_, i) => ({
      subject: `${s?.display_name ?? "Sender"} — message ${n - i}`,
      date_ms: now - i * DAY,
    }));
  },
  outcomes: () => [],
  data_location: () => "~/.local/share/dev.hush.desktop/hush.sqlite3 (demo)",
  cancel_scan: () => undefined,
  open_link: (a) => {
    console.info("[demo] would open", a.url);
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
  run_unsubscribe: (a) =>
    run(
      (a.selection as { addresses: string[] }).addresses,
      a.unsubscribe !== false,
      Boolean(a.deleteBacklog),
      Boolean(a.blockFuture),
      (a.blockAction as BlockAction) ?? "archive"
    ),
  diagnose: () => [
    { name: "Your Google key", status: "ok", detail: "Saved on this computer.", fix: "" },
    { name: "Connection", status: "ok", detail: "Connected as you@example.com.", fix: "" },
    { name: "Reading your mail", status: "ok", detail: "Granted.", fix: "" },
    {
      name: "Managing filters",
      status: "warn",
      detail: "Not granted, so Hush can't block senders.",
      fix: "Press Reconnect and tick it on Google's page.",
    },
    { name: "Reading works", status: "ok", detail: "Gmail answered — 18,442 messages in the account.", fix: "" },
    { name: "Password store", status: "ok", detail: "Working, so the connection survives quitting.", fix: "" },
    { name: "Local data", status: "ok", detail: "Readable, 12.4 MB.", fix: "" },
  ],
  list_blocks: () => demoFilters,
  preview_block_removal: (a) => {
    const f = demoFilters.find((x) => x.id === a.id);
    return {
      address: f?.address ?? "",
      action: f?.action ?? null,
      in_trash: f?.action === "trash" ? 34 : 0,
      archived: f?.action === "archive" ? 61 : 0,
      approximate: false,
    };
  },
  remove_block: (a) => {
    const f = demoFilters.find((x) => x.id === a.id);
    if (!f?.mine) throw new Error("That filter wasn't created by Hush");
    demoFilters = demoFilters.filter((x) => x.id !== a.id);
    return {
      filter_removed: true,
      restored: a.restore ? (f.action === "trash" ? 34 : 61) : 0,
      restore_failed: 0,
      problem: null,
    };
  },
  start_scan: () => {
    // Both passes, so the progress screen can be looked at as it really behaves.
    const total = 18_442;
    let found = 0;
    let scanned = 0;

    const counting = window.setInterval(() => {
      found = Math.min(total, found + 2500);
      emit("scan-progress", {
        scanned: 0,
        total: 0,
        counting: true,
        found,
        senders_found: 0,
        finished: false,
        cancelled: false,
        note: null,
      });
      if (found >= total) {
        window.clearInterval(counting);
        const reading = window.setInterval(() => {
          scanned = Math.min(total, scanned + 900);
          const finished = scanned >= total;
          if (finished) window.clearInterval(reading);
          emit("scan-progress", {
            scanned,
            total,
            counting: false,
            found: total,
            senders_found: finished ? senders.length : 0,
            finished,
            cancelled: false,
            note: null,
          });
        }, 200);
      }
    }, 180);
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
