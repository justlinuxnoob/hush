//! Heuristics that flag senders whose mail *looks* transactional even though it
//! carries an unsubscribe header.
//!
//! These never block anything. The `List-Unsubscribe` header is the real gate;
//! this module only decides whether the UI shows a warning and demands a second
//! confirmation. A false positive costs the user one extra click. A false
//! negative costs them a receipt they wanted, so the rules lean towards
//! flagging.
//!
//! Every flag carries a plain-language reason, because a warning the user
//! cannot understand is just noise.

use serde::{Deserialize, Serialize};

/// Score at or above which a sender is shown with a warning.
const CAUTION_THRESHOLD: u32 = 60;

/// The share of a sender's recent subjects that must look transactional before
/// subject wording alone raises a flag.
const SUBJECT_RATIO_THRESHOLD: f32 = 0.34;

/// What the caller knows about a sender when asking for an assessment.
#[derive(Debug, Clone)]
pub struct SenderSignals<'a> {
    /// Normalised address, e.g. `receipts@example.com`.
    pub address: &'a str,
    /// Most recent display name, used only as a weak extra signal.
    pub display_name: &'a str,
    /// A sample of subject lines from this sender.
    pub subjects: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assessment {
    /// True when the UI should warn and require a second confirmation.
    pub caution: bool,
    pub score: u32,
    /// Plain-language explanations, ready to show. No jargon.
    pub reasons: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Match {
    /// The token must be a whole dot-separated label. Used for short tokens
    /// like `ups`, which would otherwise match `startups.com` or `groups.io`.
    Label,
    /// The token may appear anywhere in the domain. Only for tokens long and
    /// distinctive enough that a chance collision is unlikely.
    Substring,
}

struct DomainRule {
    tokens: &'static [&'static str],
    mode: Match,
    weight: u32,
    reason: &'static str,
}

/// Sender families whose mail is usually something the user needs.
///
/// The lists are intentionally incomplete — they cannot be otherwise. They
/// exist to catch the most damaging mistakes, not to be exhaustive.
const DOMAIN_RULES: &[DomainRule] = &[
    DomainRule {
        tokens: &[
            "paypal",
            "stripe",
            "venmo",
            "wise",
            "revolut",
            "monzo",
            "starling",
            "chase",
            "bankofamerica",
            "wellsfargo",
            "citibank",
            "americanexpress",
            "capitalone",
            "barclays",
            "santander",
            "hsbc",
            "lloydsbank",
            "natwest",
            "nationwide",
            "schwab",
            "fidelity",
            "vanguard",
            "robinhood",
            "coinbase",
            "kraken",
            "binance",
            "sofi",
            "chime",
            "klarna",
            "afterpay",
            "affirm",
            "quickbooks",
            "xero",
            "freshbooks",
        ],
        mode: Match::Substring,
        weight: 100,
        reason: "Looks like a bank or payment service",
    },
    DomainRule {
        tokens: &["amex", "citi", "bbva", "ing", "kbc", "sepa"],
        mode: Match::Label,
        weight: 100,
        reason: "Looks like a bank or payment service",
    },
    DomainRule {
        tokens: &[
            "fedex",
            "royalmail",
            "postnl",
            "deutschepost",
            "canadapost",
            "auspost",
            "correos",
            "poste",
            "aramex",
            "yodel",
            "evri",
            "hermes",
            "shipstation",
            "aftership",
            "parcelforce",
            "purolator",
            "onetrust",
        ],
        mode: Match::Substring,
        weight: 80,
        reason: "Looks like a delivery or shipping service",
    },
    DomainRule {
        tokens: &["ups", "usps", "dhl", "dpd", "gls", "tnt", "sda"],
        mode: Match::Label,
        weight: 80,
        reason: "Looks like a delivery or shipping service",
    },
    DomainRule {
        tokens: &[
            "lufthansa",
            "ryanair",
            "easyjet",
            "airfrance",
            "britishairways",
            "emirates",
            "qatarairways",
            "turkishairlines",
            "aegeanair",
            "wizzair",
            "vueling",
            "iberia",
            "united",
            "delta",
            "southwest",
            "jetblue",
            "booking",
            "airbnb",
            "expedia",
            "trainline",
            "eurostar",
            "trenitalia",
            "renfe",
            "amtrak",
        ],
        mode: Match::Substring,
        weight: 70,
        reason: "Looks like an airline or travel booking",
    },
    DomainRule {
        tokens: &["klm", "sas", "tap", "ana", "jal"],
        mode: Match::Label,
        weight: 70,
        reason: "Looks like an airline or travel booking",
    },
    DomainRule {
        tokens: &["hmrc", "dvla", "gov", "europa", "irs", "ssa", "cra-arc"],
        mode: Match::Label,
        weight: 100,
        reason: "Looks like a government or tax service",
    },
    DomainRule {
        tokens: &["gov.uk", "gov.au", ".gov", "gouv.fr", "bund.de"],
        mode: Match::Substring,
        weight: 100,
        reason: "Looks like a government or tax service",
    },
    DomainRule {
        tokens: &[
            "mychart",
            "kaiserpermanente",
            "walgreens",
            "healthcare",
            "pharmacy",
            "hospital",
            "clinic",
            "dentist",
            "medicare",
            "medicaid",
            "bluecross",
            "unitedhealth",
            "cigna",
            "aetna",
        ],
        mode: Match::Substring,
        weight: 90,
        reason: "Looks like a health or medical service",
    },
    DomainRule {
        tokens: &["nhs", "cvs", "gsk"],
        mode: Match::Label,
        weight: 90,
        reason: "Looks like a health or medical service",
    },
    DomainRule {
        tokens: &[
            "utility",
            "energy",
            "electric",
            "waterboard",
            "council",
            "insurance",
            "mortgage",
            "landlord",
            "propertyme",
            "rentcafe",
        ],
        mode: Match::Substring,
        weight: 70,
        reason: "Looks like a bill or account provider",
    },
];

/// Mailbox names that suggest the account sends records rather than marketing.
const LOCALPART_RULES: &[(&[&str], u32, &str)] = &[
    (
        &[
            "receipt",
            "receipts",
            "invoice",
            "invoices",
            "billing",
            "bills",
            "statement",
            "statements",
            "payment",
            "payments",
            "orders",
            "order",
            "accounts",
            "account",
        ],
        70,
        "Sends from an address used for receipts or billing",
    ),
    (
        &[
            "security",
            "verify",
            "verification",
            "auth",
            "2fa",
            "otp",
            "login",
            "signin",
            "password",
            "alerts",
            "alert",
            "notification",
            "notifications",
        ],
        75,
        "Sends from an address used for security or sign-in messages",
    ),
    (
        &["support", "help", "service", "customercare", "care"],
        35,
        "Sends from a customer support address",
    ),
];

/// Subject wording that suggests a record the user may need to keep.
const SUBJECT_KEYWORDS: &[&str] = &[
    "receipt",
    "invoice",
    "your order",
    "order #",
    "order confirmation",
    "order has shipped",
    "has shipped",
    "out for delivery",
    "tracking number",
    "statement",
    "payment received",
    "payment due",
    "payment failed",
    "transaction",
    "refund",
    "your booking",
    "booking confirmation",
    "itinerary",
    "boarding pass",
    "check-in",
    "reservation",
    "appointment",
    "your ticket",
    "confirm your",
    "confirmation code",
    "activate your account",
    "welcome to",
    "your subscription",
    "renewal",
    "expires",
    "overdue",
];

/// Subject wording that is a strong signal on its own — one of these is enough.
const SUBJECT_CRITICAL: &[&str] = &[
    "verification code",
    "verify your email",
    "verify your account",
    "reset your password",
    "password reset",
    "one-time code",
    "one time passcode",
    "security code",
    "security alert",
    "sign-in attempt",
    "new sign-in",
    "new device",
    "suspicious activity",
    "two-factor",
    "your code is",
    "confirm your identity",
    "account locked",
    "unusual activity",
];

/// Assess a sender. Cheap and deterministic — no network, no model, no state.
pub fn assess(signals: &SenderSignals<'_>) -> Assessment {
    let mut score = 0u32;
    let mut reasons: Vec<String> = Vec::new();

    let address = signals.address.to_ascii_lowercase();
    let (local, domain) = split_address(&address);

    for rule in DOMAIN_RULES {
        if rule
            .tokens
            .iter()
            .any(|t| domain_matches(domain, t, rule.mode))
        {
            score += rule.weight;
            push_reason(&mut reasons, rule.reason);
        }
    }

    for (tokens, weight, reason) in LOCALPART_RULES {
        if tokens.iter().any(|t| localpart_matches(local, t)) {
            score += weight;
            push_reason(&mut reasons, reason);
        }
    }

    score += score_subjects(signals.subjects, &mut reasons);

    // The display name is a weak signal on its own, so it only nudges.
    let name = signals.display_name.to_ascii_lowercase();
    if ["receipt", "billing", "invoice", "security", "support"]
        .iter()
        .any(|t| name.contains(t))
    {
        score += 25;
        push_reason(
            &mut reasons,
            "The sender's name mentions receipts or account matters",
        );
    }

    Assessment {
        caution: score >= CAUTION_THRESHOLD,
        score,
        reasons,
    }
}

fn score_subjects(subjects: &[String], reasons: &mut Vec<String>) -> u32 {
    if subjects.is_empty() {
        return 0;
    }
    let lowered: Vec<String> = subjects.iter().map(|s| s.to_ascii_lowercase()).collect();

    if lowered
        .iter()
        .any(|s| SUBJECT_CRITICAL.iter().any(|k| s.contains(k)))
    {
        push_reason(
            reasons,
            "Recent messages look like sign-in or security codes",
        );
        return 95;
    }

    let hits = lowered
        .iter()
        .filter(|s| SUBJECT_KEYWORDS.iter().any(|k| s.contains(k)))
        .count();

    if hits == 0 {
        return 0;
    }

    let ratio = hits as f32 / lowered.len() as f32;
    if ratio >= SUBJECT_RATIO_THRESHOLD {
        push_reason(
            reasons,
            "Recent messages mention things like orders, receipts or bookings",
        );
        70
    } else {
        // A minority of matching subjects is common for shops that send both
        // marketing and receipts, so it contributes without flagging alone.
        30
    }
}

fn push_reason(reasons: &mut Vec<String>, reason: &str) {
    if !reasons.iter().any(|r| r == reason) {
        reasons.push(reason.to_string());
    }
}

fn split_address(address: &str) -> (&str, &str) {
    match address.split_once('@') {
        Some((l, d)) => (l, d),
        None => ("", address),
    }
}

fn domain_matches(domain: &str, token: &str, mode: Match) -> bool {
    match mode {
        Match::Substring => domain.contains(token),
        Match::Label => domain.split('.').any(|label| label == token),
    }
}

/// Match a mailbox name against a token on word-ish boundaries, so that
/// `no-reply` does not match `reply` inside `replies-to-newsletter`.
fn localpart_matches(local: &str, token: &str) -> bool {
    local
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| part == token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals<'a>(address: &'a str, subjects: &'a [String]) -> SenderSignals<'a> {
        SenderSignals {
            address,
            display_name: "",
            subjects,
        }
    }

    fn subj(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plain_newsletters_are_not_flagged() {
        let none: Vec<String> = vec![];
        for addr in [
            "news@substack.com",
            "hello@brandnewsletter.com",
            "newsletter@theverge.com",
            "weekly@indiehackers.com",
            "digest@medium.com",
            "no-reply@notifications.figma.com",
        ] {
            let a = assess(&signals(addr, &none));
            assert!(
                !a.caution,
                "{addr} should not be flagged (score {})",
                a.score
            );
        }
    }

    #[test]
    fn marketing_subjects_are_not_flagged() {
        let s = subj(&[
            "50% off everything this weekend",
            "New arrivals just dropped",
            "Your weekly reading list",
            "Last chance: sale ends tonight",
        ]);
        let a = assess(&signals("shop@example.com", &s));
        assert!(!a.caution, "score {}", a.score);
    }

    #[test]
    fn banks_and_payment_services_are_flagged() {
        let none: Vec<String> = vec![];
        for addr in [
            "service@paypal.com",
            "noreply@e.chase.com",
            "alerts@wellsfargo.com",
            "no-reply@stripe.com",
            "info@revolut.com",
            "statements@amex.com",
        ] {
            let a = assess(&signals(addr, &none));
            assert!(a.caution, "{addr} should be flagged");
            assert!(!a.reasons.is_empty());
        }
    }

    #[test]
    fn delivery_services_are_flagged() {
        let none: Vec<String> = vec![];
        for addr in ["tracking@ups.com", "no-reply@fedex.com", "info@dhl.de"] {
            assert!(assess(&signals(addr, &none)).caution, "{addr}");
        }
    }

    #[test]
    fn short_tokens_do_not_match_inside_unrelated_words() {
        // `ups` must not match these. This is the failure mode the Label match
        // mode exists to prevent.
        let none: Vec<String> = vec![];
        for addr in [
            "hello@startups.com",
            "digest@groups.io",
            "team@meetups.example.com",
            "news@sasquatch.com",
        ] {
            let a = assess(&signals(addr, &none));
            assert!(!a.caution, "{addr} wrongly flagged (score {})", a.score);
        }
    }

    #[test]
    fn government_and_health_are_flagged() {
        let none: Vec<String> = vec![];
        for addr in [
            "noreply@hmrc.gov.uk",
            "no-reply@mychart.example.org",
            "info@nhs.uk",
        ] {
            assert!(assess(&signals(addr, &none)).caution, "{addr}");
        }
    }

    #[test]
    fn security_codes_flag_on_subject_alone() {
        let s = subj(&["Your verification code is 123456"]);
        let a = assess(&signals("hello@somestartup.io", &s));
        assert!(a.caution);
        assert!(a.reasons.iter().any(|r| r.contains("sign-in")));
    }

    #[test]
    fn a_password_reset_flags_even_among_marketing() {
        let s = subj(&[
            "Big sale today",
            "New in store",
            "Reset your password",
            "Weekly picks",
        ]);
        assert!(assess(&signals("hello@shop.example", &s)).caution);
    }

    #[test]
    fn mostly_order_subjects_are_flagged() {
        let s = subj(&[
            "Your order #1234 has shipped",
            "Your order #1235 has shipped",
            "Order confirmation",
            "Spring lookbook",
        ]);
        let a = assess(&signals("hello@shop.example", &s));
        assert!(a.caution, "score {}", a.score);
    }

    #[test]
    fn a_single_receipt_among_many_promos_does_not_flag_alone() {
        let s = subj(&[
            "Your receipt from us",
            "Sale starts now",
            "New arrivals",
            "Weekend inspiration",
            "Meet the team",
            "Style guide",
        ]);
        let a = assess(&signals("hello@shop.example", &s));
        assert!(!a.caution, "score {}", a.score);
    }

    #[test]
    fn receipts_mailbox_is_flagged() {
        let none: Vec<String> = vec![];
        for addr in [
            "receipts@example.com",
            "billing@example.com",
            "invoice-noreply@example.com",
            "security@example.com",
        ] {
            assert!(assess(&signals(addr, &none)).caution, "{addr}");
        }
    }

    #[test]
    fn support_alone_is_a_nudge_not_a_flag() {
        let none: Vec<String> = vec![];
        let a = assess(&signals("support@example.com", &none));
        assert!(!a.caution, "score {}", a.score);
        assert!(a.score > 0);
    }

    #[test]
    fn localpart_matching_respects_boundaries() {
        assert!(localpart_matches("no-reply-billing", "billing"));
        assert!(localpart_matches("billing", "billing"));
        assert!(localpart_matches("shop.orders", "orders"));
        assert!(!localpart_matches("reorders", "orders"));
        assert!(!localpart_matches("securityblog", "security"));
    }

    #[test]
    fn reasons_are_deduplicated() {
        let none: Vec<String> = vec![];
        let a = assess(&signals("billing-receipts@paypal.com", &none));
        let mut sorted = a.reasons.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), a.reasons.len());
    }

    #[test]
    fn display_name_nudges_but_reads_as_plain_language() {
        let none: Vec<String> = vec![];
        let a = assess(&SenderSignals {
            address: "hello@example.com",
            display_name: "Example Billing",
            subjects: &none,
        });
        assert!(a.score > 0);
        for r in &a.reasons {
            for jargon in ["header", "API", "token", "regex"] {
                assert!(!r.contains(jargon), "reason contains jargon: {r}");
            }
        }
    }
}
