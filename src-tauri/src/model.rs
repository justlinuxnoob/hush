//! Types shared between the Rust core and the interface.

use serde::{Deserialize, Serialize};

use crate::heuristics::Assessment;
use crate::parse::UnsubMethod;

/// The metadata Hush keeps for one message.
///
/// Note what is absent: the message body. Hush asks Gmail for
/// `format=metadata` and a fixed list of header names, so the body is never
/// sent over the wire and cannot be stored here even by accident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageMeta {
    pub id: String,
    pub sender_address: String,
    pub sender_name: String,
    pub subject: String,
    /// Epoch milliseconds, taken from Gmail's `internalDate`.
    pub date_ms: i64,
    pub list_unsubscribe: Option<String>,
    pub list_unsubscribe_post: Option<String>,
}

/// How deep a scan should go. A fast first run beats a perfect one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanDepth {
    SixMonths,
    OneYear,
    TwoYears,
    Everything,
}

impl ScanDepth {
    /// The Gmail search term for this depth, or none for "everything".
    pub fn query_fragment(&self) -> Option<&'static str> {
        match self {
            ScanDepth::SixMonths => Some("newer_than:6m"),
            ScanDepth::OneYear => Some("newer_than:1y"),
            ScanDepth::TwoYears => Some("newer_than:2y"),
            ScanDepth::Everything => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ScanDepth::SixMonths => "the last 6 months",
            ScanDepth::OneYear => "the last year",
            ScanDepth::TwoYears => "the last 2 years",
            ScanDepth::Everything => "everything",
        }
    }
}

/// Progress for the scanning screen. Counts are real, not estimated
/// percentages, because a progress bar that lies is worse than no bar.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanProgress {
    pub scanned: u64,
    /// Gmail's own estimate of the total. It is an estimate and is labelled as
    /// one in the interface.
    pub total_estimate: u64,
    pub senders_found: u64,
    pub finished: bool,
    pub cancelled: bool,
    /// Set when the scan stopped early for a reason worth telling the user.
    pub note: Option<String>,
}

/// What happened when we tried to unsubscribe from a sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    /// Fully done, no further action needed.
    Done,
    /// We handed off to the user's mail app or sent a mail; delivery is not
    /// something we can confirm, so this is deliberately not `Done`.
    Sent,
    /// The user must open a link themselves.
    NeedsYou,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub address: String,
    pub display_name: String,
    pub status: OutcomeStatus,
    /// Plain-language detail, e.g. "The website didn't respond".
    pub detail: String,
    /// Present when the user needs to open something.
    pub link: Option<String>,
    pub at_ms: i64,
}

/// One sender, as the list screen sees them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    /// The normalised address; also the identity used everywhere else.
    pub address: String,
    pub display_name: String,
    pub message_count: u32,
    /// How many of those carried an unsubscribe header — the ones the tidy-up
    /// feature would move to Trash. Always at most `message_count`; the
    /// difference is receipts and one-off mail, which are never touched.
    pub bulk_count: u32,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
    /// Human-readable cadence, e.g. "about 4 a week".
    pub frequency: String,
    pub method: UnsubMethod,
    /// Every route this sender offers, best first. `method` is the first of
    /// them; the rest are tried only if the earlier ones fail.
    pub fallbacks: Vec<UnsubMethod>,
    pub assessment: Assessment,
    pub never_touch: bool,
    /// The most recent outcome for this sender, if we have acted before.
    pub outcome: Option<Outcome>,
    /// A few recent subject lines, so the user can recognise the sender.
    pub sample_subjects: Vec<String>,
}

/// Describe how often a sender writes, in words rather than numbers.
///
/// Cadence is only meaningful over a span, so a sender seen once — or seen many
/// times in a single day — is described plainly instead of being extrapolated
/// into a nonsense rate like "about 300 a week".
pub fn describe_frequency(count: u32, first_ms: i64, last_ms: i64) -> String {
    if count <= 1 {
        return "once".to_string();
    }
    let span_days = ((last_ms - first_ms).max(0) as f64) / 86_400_000.0;
    if span_days < 1.0 {
        return format!("{count} in one day");
    }

    // Gaps between messages, not messages per day: n messages span n-1 gaps.
    let per_day = (count as f64 - 1.0) / span_days;
    let per_week = per_day * 7.0;

    if per_day >= 1.5 {
        format!("about {} a day", per_day.round() as u64)
    } else if per_week >= 1.5 {
        format!("about {} a week", per_week.round() as u64)
    } else if per_week >= 0.4 {
        "about weekly".to_string()
    } else {
        let per_month = per_day * 30.0;
        if per_month >= 1.5 {
            format!("about {} a month", per_month.round() as u64)
        } else if per_month >= 0.5 {
            "about monthly".to_string()
        } else {
            "a few times a year".to_string()
        }
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Reduce a `From` header to a comparable identity.
///
/// Grouping is by address, never by display name: display names change with
/// every campaign ("Anna from Acme", "Acme Weekly", "Acme 🎁") while the
/// address stays put. Gmail's `+tag` suffixes and case differences are folded
/// away so one sender does not appear as five.
pub fn normalise_address(raw: &str) -> String {
    let addr = extract_address(raw).to_ascii_lowercase();
    let Some((local, domain)) = addr.split_once('@') else {
        return addr;
    };
    let local = local.split('+').next().unwrap_or(local);
    format!("{local}@{domain}")
}

/// Pull the address out of a `From` header value.
///
/// Handles `Name <a@b.c>`, `"Quoted, Name" <a@b.c>` and a bare `a@b.c`.
pub fn extract_address(raw: &str) -> String {
    if let Some(start) = raw.rfind('<') {
        if let Some(end) = raw[start..].find('>') {
            return raw[start + 1..start + end].trim().to_string();
        }
    }
    raw.trim().trim_matches('"').to_string()
}

/// Pull the display name out of a `From` header, falling back to the part of
/// the address before the `@` so there is always something readable to show.
pub fn extract_display_name(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(start) = raw.rfind('<') {
        let name = raw[..start].trim().trim_matches('"').trim();
        if !name.is_empty() {
            return decode_mime_word_ascii(name);
        }
    }
    let addr = extract_address(raw);
    addr.split('@').next().unwrap_or(&addr).to_string()
}

/// Decode the plain-ASCII cases of RFC 2047 encoded words.
///
/// Gmail already decodes most display names for us; this only rescues the
/// occasional `=?utf-8?q?...?=` that slips through, and leaves anything it
/// does not understand exactly as it found it rather than mangling it.
fn decode_mime_word_ascii(s: &str) -> String {
    if !s.starts_with("=?") || !s.ends_with("?=") {
        return s.to_string();
    }
    let inner = &s[2..s.len() - 2];
    let parts: Vec<&str> = inner.splitn(3, '?').collect();
    let [charset, encoding, payload] = parts[..] else {
        return s.to_string();
    };
    if !charset.eq_ignore_ascii_case("utf-8") && !charset.eq_ignore_ascii_case("us-ascii") {
        return s.to_string();
    }

    let bytes = match encoding.to_ascii_lowercase().as_str() {
        "b" => {
            use base64::Engine as _;
            match base64::engine::general_purpose::STANDARD.decode(payload) {
                Ok(b) => b,
                Err(_) => return s.to_string(),
            }
        }
        "q" => {
            let mut out = Vec::with_capacity(payload.len());
            let raw = payload.as_bytes();
            let mut i = 0;
            while i < raw.len() {
                match raw[i] {
                    b'_' => {
                        out.push(b' ');
                        i += 1;
                    }
                    b'=' if i + 2 < raw.len() => {
                        let hex = std::str::from_utf8(&raw[i + 1..i + 3]).unwrap_or("");
                        match u8::from_str_radix(hex, 16) {
                            Ok(b) => out.push(b),
                            Err(_) => return s.to_string(),
                        }
                        i += 3;
                    }
                    b => {
                        out.push(b);
                        i += 1;
                    }
                }
            }
            out
        }
        _ => return s.to_string(),
    };

    String::from_utf8(bytes).unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;

    #[test]
    fn addresses_are_extracted_from_from_headers() {
        assert_eq!(extract_address("Acme <news@acme.com>"), "news@acme.com");
        assert_eq!(
            extract_address("\"Acme, Inc.\" <news@acme.com>"),
            "news@acme.com"
        );
        assert_eq!(extract_address("news@acme.com"), "news@acme.com");
        assert_eq!(extract_address("  news@acme.com  "), "news@acme.com");
        // A display name containing an '<' must not confuse the extraction.
        assert_eq!(extract_address("a <b> c <news@acme.com>"), "news@acme.com");
    }

    #[test]
    fn normalisation_folds_case_and_plus_tags() {
        assert_eq!(normalise_address("News@Acme.COM"), "news@acme.com");
        assert_eq!(
            normalise_address("Acme <news+campaign99@acme.com>"),
            "news@acme.com"
        );
        assert_eq!(
            normalise_address("Acme Weekly <NEWS+a@Acme.com>"),
            normalise_address("Anna from Acme <news+b@acme.com>")
        );
    }

    #[test]
    fn display_names_fall_back_to_the_mailbox() {
        assert_eq!(extract_display_name("Acme <news@acme.com>"), "Acme");
        assert_eq!(extract_display_name("<news@acme.com>"), "news");
        assert_eq!(extract_display_name("news@acme.com"), "news");
        assert_eq!(
            extract_display_name("\"Acme, Inc.\" <a@b.com>"),
            "Acme, Inc."
        );
    }

    #[test]
    fn encoded_display_names_are_decoded_or_left_alone() {
        assert_eq!(
            extract_display_name("=?utf-8?q?Acme_Weekly?= <a@b.com>"),
            "Acme Weekly"
        );
        assert_eq!(
            extract_display_name("=?utf-8?B?QWNtZQ==?= <a@b.com>"),
            "Acme"
        );
        // Unsupported charsets are passed through untouched rather than mangled.
        let exotic = "=?iso-8859-7?q?abc?=";
        assert_eq!(extract_display_name(&format!("{exotic} <a@b.com>")), exotic);
    }

    #[test]
    fn frequency_reads_as_english() {
        assert_eq!(describe_frequency(1, 0, 0), "once");
        assert_eq!(describe_frequency(5, 0, DAY / 2), "5 in one day");
        assert_eq!(describe_frequency(8, 0, 7 * DAY), "about 7 a week");
        assert_eq!(describe_frequency(5, 0, 28 * DAY), "about weekly");
        assert_eq!(describe_frequency(4, 0, 90 * DAY), "about monthly");
        assert_eq!(describe_frequency(3, 0, 365 * DAY), "a few times a year");
        assert_eq!(describe_frequency(60, 0, 30 * DAY), "about 2 a day");
    }

    #[test]
    fn frequency_never_extrapolates_a_single_day_into_a_rate() {
        // The bug this guards against: 300 messages in one day reported as
        // "about 2100 a week".
        let f = describe_frequency(300, 0, DAY / 4);
        assert_eq!(f, "300 in one day");
    }
}
