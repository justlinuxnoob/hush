//! Parsing of the `List-Unsubscribe` (RFC 2369) and `List-Unsubscribe-Post`
//! (RFC 8058) header fields.
//!
//! This module is the safety gate for the whole application: a sender is only
//! ever offered as unsubscribable because something here said their mail
//! carries a `List-Unsubscribe` field. Transactional mail — receipts, password
//! resets, 2FA codes — generally carries no such field, so getting this parser
//! right is what keeps the app from touching mail the user needs.
//!
//! Real-world headers violate the grammar constantly, so the parser is
//! deliberately lenient about *shape* (missing brackets, odd separators, folded
//! lines) and deliberately strict about *meaning* (a URI must parse, and
//! one-click requires HTTPS). Leniency that produces a bogus URI is a bug;
//! leniency that recovers a real one is the point.

use std::collections::HashSet;

use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use url::Url;

/// A single usable target extracted from a `List-Unsubscribe` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnsubTarget {
    /// An `https://` endpoint. Safe to POST to *only* when one-click is advertised.
    Https(String),
    /// A plaintext `http://` endpoint. Never used automatically — manual only.
    HttpInsecure(String),
    /// A `mailto:` target, with any `subject=` / `body=` parameters decoded.
    Mailto {
        address: String,
        subject: Option<String>,
        body: Option<String>,
    },
}

/// What we are willing to do for a given sender, in descending order of
/// preference. This is the only type the rest of the app acts on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnsubMethod {
    /// RFC 8058: POST `List-Unsubscribe=One-Click` and we are done. Fully automatic.
    OneClick { url: String },
    /// Send a mail to the given address.
    Mailto {
        address: String,
        subject: Option<String>,
        body: Option<String>,
    },
    /// A link with no one-click support. The human opens it themselves; we never
    /// auto-POST or auto-GET these, because a bare link can mean anything.
    ManualLink { url: String },
    /// The header existed but nothing usable came out of it.
    None,
}

impl UnsubMethod {
    pub fn is_actionable(&self) -> bool {
        !matches!(self, UnsubMethod::None)
    }
}

impl ParsedUnsub {
    /// Every way this sender offers to unsubscribe, best first.
    ///
    /// Senders commonly publish both a `mailto:` and an HTTPS endpoint. Picking
    /// the best one and discarding the rest means a sender who offers two
    /// routes gets reported as a failure when the first one happens to be down
    /// — so the caller gets the whole list and works down it.
    pub fn methods(&self) -> Vec<UnsubMethod> {
        let mut out = Vec::new();

        if self.one_click_advertised {
            if let Some(UnsubTarget::Https(url)) = self
                .targets
                .iter()
                .find(|t| matches!(t, UnsubTarget::Https(_)))
            {
                out.push(UnsubMethod::OneClick { url: url.clone() });
            }
        }

        for t in &self.targets {
            if let UnsubTarget::Mailto {
                address,
                subject,
                body,
            } = t
            {
                out.push(UnsubMethod::Mailto {
                    address: address.clone(),
                    subject: subject.clone(),
                    body: body.clone(),
                });
            }
        }

        // Links come last, and only ever as something for the human to open.
        for t in &self.targets {
            match t {
                UnsubTarget::Https(url) | UnsubTarget::HttpInsecure(url) => {
                    let manual = UnsubMethod::ManualLink { url: url.clone() };
                    if !out.contains(&manual) {
                        out.push(manual);
                    }
                }
                UnsubTarget::Mailto { .. } => {}
            }
        }

        out
    }
}

/// Everything we learned from one message's unsubscribe headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedUnsub {
    /// Every target we could recover, in header order.
    pub targets: Vec<UnsubTarget>,
    /// Whether a valid `List-Unsubscribe-Post` field was present.
    pub one_click_advertised: bool,
    /// The single method we would actually use.
    pub method: UnsubMethod,
}

/// Parse the `List-Unsubscribe` and `List-Unsubscribe-Post` header values.
///
/// Both arguments are the raw field bodies (without the field name). Pass
/// `None` for `post` when the message had no `List-Unsubscribe-Post` field.
pub fn parse_unsubscribe(list_unsubscribe: Option<&str>, post: Option<&str>) -> ParsedUnsub {
    let targets = list_unsubscribe.map(parse_targets).unwrap_or_default();
    let one_click_advertised = post.is_some_and(is_one_click_post);
    let method = choose_method(&targets, one_click_advertised);
    ParsedUnsub {
        targets,
        one_click_advertised,
        method,
    }
}

/// Decide the single action to take.
///
/// One-click wins when it is genuinely available; RFC 8058 requires an HTTPS
/// URI, so an `http://` link never qualifies no matter what the POST header
/// claims. After that we prefer `mailto:` over a bare link, because a mailto is
/// unambiguous while an unadorned link may be a preference centre, a login
/// wall, or a one-tap unsubscribe — we cannot tell, so a human decides.
fn choose_method(targets: &[UnsubTarget], one_click: bool) -> UnsubMethod {
    if one_click {
        if let Some(UnsubTarget::Https(url)) =
            targets.iter().find(|t| matches!(t, UnsubTarget::Https(_)))
        {
            return UnsubMethod::OneClick { url: url.clone() };
        }
    }

    if let Some(UnsubTarget::Mailto {
        address,
        subject,
        body,
    }) = targets
        .iter()
        .find(|t| matches!(t, UnsubTarget::Mailto { .. }))
    {
        return UnsubMethod::Mailto {
            address: address.clone(),
            subject: subject.clone(),
            body: body.clone(),
        };
    }

    for t in targets {
        match t {
            UnsubTarget::Https(url) | UnsubTarget::HttpInsecure(url) => {
                return UnsubMethod::ManualLink { url: url.clone() }
            }
            UnsubTarget::Mailto { .. } => {}
        }
    }

    UnsubMethod::None
}

/// `List-Unsubscribe-Post` must carry exactly the pair `List-Unsubscribe=One-Click`.
///
/// We accept case differences and whitespace around the `=` because senders get
/// this wrong, but we do not accept extra pairs or a different value — those
/// are not RFC 8058 and POSTing to them would be guesswork.
fn is_one_click_post(value: &str) -> bool {
    let unfolded = unfold(value);
    let mut parts = unfolded.split('=');
    let (Some(key), Some(val), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    key.trim().eq_ignore_ascii_case("List-Unsubscribe")
        && val.trim().eq_ignore_ascii_case("One-Click")
}

/// Turn a folded header body into a single line.
///
/// RFC 5322 folding inserts CRLF before whitespace; unfolding is removing that
/// CRLF. We also normalise bare CR/LF, which appear when headers have been
/// mangled by an intermediate system.
fn unfold(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' || c == '\n' {
            // Collapse any run of CR/LF plus the following whitespace into one space.
            while matches!(chars.peek(), Some('\r' | '\n' | ' ' | '\t')) {
                chars.next();
            }
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Extract every candidate URI from a `List-Unsubscribe` field body.
fn parse_targets(value: &str) -> Vec<UnsubTarget> {
    let unfolded = unfold(value);
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for raw in split_candidates(&unfolded) {
        let Some(target) = classify(&raw) else {
            continue;
        };
        let key = dedupe_key(&target);
        if seen.insert(key) {
            out.push(target);
        }
    }
    out
}

/// Split a field body into candidate URI strings.
///
/// Angle-bracketed runs are taken verbatim (a URI may legally contain commas
/// and semicolons, so we must not split inside brackets). Text outside brackets
/// is split on the separators senders actually use, plus whitespace — no valid
/// URI contains unescaped whitespace, so that split is always safe.
fn split_candidates(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut chars = value.chars().peekable();
    let mut paren_depth = 0usize;

    while let Some(c) = chars.next() {
        match c {
            '<' => {
                flush(&mut buf, &mut out);
                let mut inner = String::new();
                // An unterminated '<' runs to the end of the field rather than
                // discarding the URI, which is what a truncated header needs.
                for c in chars.by_ref() {
                    if c == '>' {
                        break;
                    }
                    inner.push(c);
                }
                flush(&mut inner, &mut out);
            }
            // RFC 5322 comments live outside the brackets; parentheses inside a
            // URI are handled above because that text never reaches here.
            '(' => {
                paren_depth += 1;
                flush(&mut buf, &mut out);
            }
            ')' => {
                paren_depth = paren_depth.saturating_sub(1);
                buf.clear();
            }
            _ if paren_depth > 0 => {}
            ',' | ';' | ' ' | '\t' => flush(&mut buf, &mut out),
            _ => buf.push(c),
        }
    }
    flush(&mut buf, &mut out);
    out
}

fn flush(buf: &mut String, out: &mut Vec<String>) {
    let trimmed: String = buf.chars().filter(|c| !c.is_whitespace()).collect();
    if !trimmed.is_empty() {
        out.push(trimmed);
    }
    buf.clear();
}

fn dedupe_key(t: &UnsubTarget) -> String {
    match t {
        UnsubTarget::Https(u) | UnsubTarget::HttpInsecure(u) => u.to_lowercase(),
        UnsubTarget::Mailto { address, .. } => format!("mailto:{}", address.to_lowercase()),
    }
}

/// Turn one candidate string into a target, or discard it.
///
/// Anything that is not a scheme we understand, or that `url` cannot parse, is
/// dropped. Junk like a bare `unsubscribe` or `NO` ends up here and is discarded.
fn classify(raw: &str) -> Option<UnsubTarget> {
    let lower = raw.to_ascii_lowercase();

    if lower.starts_with("mailto:") {
        return parse_mailto(raw);
    }

    if lower.starts_with("https://") || lower.starts_with("http://") {
        let url = Url::parse(raw).ok()?;
        // A URL with no host is unusable (and would be a confusing thing to
        // show a user), so it does not survive.
        if url.host_str().is_none_or(|h| h.is_empty()) {
            return None;
        }
        let normalised = url.to_string();
        return if lower.starts_with("https://") {
            Some(UnsubTarget::Https(normalised))
        } else {
            Some(UnsubTarget::HttpInsecure(normalised))
        };
    }

    None
}

fn parse_mailto(raw: &str) -> Option<UnsubTarget> {
    let url = Url::parse(raw).ok()?;
    // `url` exposes a mailto's addresses as the opaque path.
    let path = url.path();
    // Multiple recipients are legal; one unsubscribe address is enough.
    let address = path.split(',').next()?.trim();
    let address = percent_decode_str(address).decode_utf8_lossy().to_string();
    if !is_plausible_address(&address) {
        return None;
    }

    let mut subject = None;
    let mut body = None;
    for (k, v) in url.query_pairs() {
        // Values arrive already percent-decoded from `query_pairs`.
        match k.to_ascii_lowercase().as_str() {
            "subject" if !v.is_empty() => subject = Some(v.to_string()),
            "body" if !v.is_empty() => body = Some(v.to_string()),
            _ => {}
        }
    }

    Some(UnsubTarget::Mailto {
        address,
        subject,
        body,
    })
}

/// A deliberately loose address check: enough to reject junk, not so strict
/// that it rejects the odd-but-real addresses bulk senders use.
fn is_plausible_address(addr: &str) -> bool {
    let mut parts = addr.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !addr.chars().any(|c| c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn https(u: &str) -> UnsubTarget {
        UnsubTarget::Https(u.to_string())
    }
    fn mailto(a: &str) -> UnsubTarget {
        UnsubTarget::Mailto {
            address: a.to_string(),
            subject: None,
            body: None,
        }
    }

    // --- Well-formed input -------------------------------------------------

    #[test]
    fn single_bracketed_https() {
        let p = parse_unsubscribe(Some("<https://example.com/u/abc>"), None);
        assert_eq!(p.targets, vec![https("https://example.com/u/abc")]);
        assert_eq!(
            p.method,
            UnsubMethod::ManualLink {
                url: "https://example.com/u/abc".into()
            }
        );
    }

    #[test]
    fn rfc8058_example_is_one_click() {
        let p = parse_unsubscribe(
            Some("<https://example.com/unsubscribe/opaquepart>"),
            Some("List-Unsubscribe=One-Click"),
        );
        assert!(p.one_click_advertised);
        assert_eq!(
            p.method,
            UnsubMethod::OneClick {
                url: "https://example.com/unsubscribe/opaquepart".into()
            }
        );
    }

    #[test]
    fn rfc8058_mixed_example_prefers_the_https_one_click() {
        let p = parse_unsubscribe(
            Some(
                "<mailto:listrequest@example.com?subject=unsubscribe>, \
                 <https://example.com/unsubscribe.html?opaque=123456789>",
            ),
            Some("List-Unsubscribe=One-Click"),
        );
        assert_eq!(p.targets.len(), 2);
        assert_eq!(
            p.method,
            UnsubMethod::OneClick {
                url: "https://example.com/unsubscribe.html?opaque=123456789".into()
            }
        );
    }

    #[test]
    fn mailto_wins_over_bare_link_without_one_click() {
        let p = parse_unsubscribe(
            Some("<https://example.com/u>, <mailto:leave@example.com>"),
            None,
        );
        assert_eq!(
            p.method,
            UnsubMethod::Mailto {
                address: "leave@example.com".into(),
                subject: None,
                body: None
            }
        );
    }

    #[test]
    fn mailto_subject_and_body_are_decoded() {
        let p = parse_unsubscribe(
            Some("<mailto:u@example.com?subject=Unsub%20me&body=please%20stop>"),
            None,
        );
        assert_eq!(
            p.method,
            UnsubMethod::Mailto {
                address: "u@example.com".into(),
                subject: Some("Unsub me".into()),
                body: Some("please stop".into()),
            }
        );
    }

    // --- Malformed but recoverable ----------------------------------------

    #[test]
    fn missing_angle_brackets() {
        let p = parse_unsubscribe(Some("https://example.com/u"), None);
        assert_eq!(p.targets, vec![https("https://example.com/u")]);
    }

    #[test]
    fn separated_by_space_without_comma() {
        let p = parse_unsubscribe(Some("<mailto:a@example.com> <https://example.com/u>"), None);
        assert_eq!(p.targets.len(), 2);
    }

    #[test]
    fn separated_by_semicolon() {
        let p = parse_unsubscribe(Some("<mailto:a@example.com>;<https://example.com/u>"), None);
        assert_eq!(p.targets.len(), 2);
    }

    #[test]
    fn folded_header_is_unfolded() {
        let p = parse_unsubscribe(
            Some("<mailto:listrequest@example.com?subject=unsubscribe>,\r\n\t<https://example.com/u>"),
            None,
        );
        assert_eq!(p.targets.len(), 2);
        assert_eq!(p.targets[1], https("https://example.com/u"));
    }

    #[test]
    fn fold_inside_a_uri_is_stitched_back_together() {
        // Some agents fold in the middle of a long URL. Whitespace is never
        // valid inside a URI, so removing it restores the original.
        let p = parse_unsubscribe(Some("<https://example.com/very/long/\r\n path?x=1>"), None);
        assert_eq!(
            p.targets,
            vec![https("https://example.com/very/long/path?x=1")]
        );
    }

    #[test]
    fn unterminated_bracket_still_yields_the_uri() {
        let p = parse_unsubscribe(Some("<https://example.com/u"), None);
        assert_eq!(p.targets, vec![https("https://example.com/u")]);
    }

    #[test]
    fn rfc5322_comment_is_ignored() {
        let p = parse_unsubscribe(
            Some("<https://example.com/u> (click here to unsubscribe)"),
            None,
        );
        assert_eq!(p.targets, vec![https("https://example.com/u")]);
    }

    #[test]
    fn duplicate_uris_are_collapsed() {
        let p = parse_unsubscribe(
            Some("<https://example.com/u>, <https://example.com/u>, <HTTPS://EXAMPLE.COM/u>"),
            None,
        );
        assert_eq!(p.targets.len(), 1);
    }

    #[test]
    fn uri_containing_a_comma_is_not_split() {
        let p = parse_unsubscribe(Some("<https://example.com/u?ids=1,2,3>"), None);
        assert_eq!(p.targets, vec![https("https://example.com/u?ids=1,2,3")]);
    }

    // --- Junk that must produce nothing ------------------------------------

    #[test]
    fn empty_value_is_not_actionable() {
        for v in ["", "   ", "<>", "<> , <>", ",,,", "\r\n "] {
            let p = parse_unsubscribe(Some(v), None);
            assert!(p.targets.is_empty(), "expected no targets for {v:?}");
            assert_eq!(p.method, UnsubMethod::None, "for {v:?}");
        }
    }

    #[test]
    fn junk_text_is_discarded() {
        for v in [
            "unsubscribe",
            "NO",
            "<unsubscribe>",
            "<ftp://example.com/u>",
            "<javascript:alert(1)>",
            "<data:text/html,hi>",
            "<https://>",
            "<mailto:>",
            "<mailto:not-an-address>",
            "<mailto:missing@domain>",
        ] {
            let p = parse_unsubscribe(Some(v), None);
            assert!(p.targets.is_empty(), "expected no targets for {v:?}");
        }
    }

    #[test]
    fn junk_alongside_a_real_uri_keeps_only_the_real_one() {
        let p = parse_unsubscribe(Some("unsubscribe, <https://example.com/u>, garbage"), None);
        assert_eq!(p.targets, vec![https("https://example.com/u")]);
    }

    // --- One-click gating --------------------------------------------------

    #[test]
    fn one_click_over_plain_http_is_refused() {
        // RFC 8058 requires HTTPS. Downgrading to a manual link is the safe
        // failure: the user can still act, we just will not POST for them.
        let p = parse_unsubscribe(
            Some("<http://example.com/u>"),
            Some("List-Unsubscribe=One-Click"),
        );
        assert!(p.one_click_advertised);
        assert_eq!(
            p.method,
            UnsubMethod::ManualLink {
                url: "http://example.com/u".into()
            }
        );
    }

    #[test]
    fn one_click_with_only_a_mailto_falls_back_to_mailto() {
        let p = parse_unsubscribe(
            Some("<mailto:leave@example.com>"),
            Some("List-Unsubscribe=One-Click"),
        );
        assert_eq!(
            p.method,
            UnsubMethod::Mailto {
                address: "leave@example.com".into(),
                subject: None,
                body: None
            }
        );
    }

    #[test]
    fn post_header_variants() {
        assert!(is_one_click_post("List-Unsubscribe=One-Click"));
        assert!(is_one_click_post("list-unsubscribe=one-click"));
        assert!(is_one_click_post("  List-Unsubscribe = One-Click  "));
        assert!(is_one_click_post("List-Unsubscribe=One-Click\r\n"));

        // Anything that is not exactly the RFC 8058 pair is not one-click.
        assert!(!is_one_click_post(""));
        assert!(!is_one_click_post("One-Click"));
        assert!(!is_one_click_post("List-Unsubscribe=Yes"));
        assert!(!is_one_click_post("List-Unsubscribe=One-Click; extra=1"));
        assert!(!is_one_click_post("List-Unsubscribe=One-Click=Two"));
    }

    #[test]
    fn every_route_a_sender_offers_is_kept_in_order() {
        // The case that matters: one-click fails, but this sender also
        // published a mailto that would work. Discarding it would report a
        // failure the sender never actually caused.
        let p = parse_unsubscribe(
            Some("<mailto:leave@acme.example>, <https://acme.example/u>"),
            Some("List-Unsubscribe=One-Click"),
        );
        let methods = p.methods();
        assert_eq!(methods.len(), 3, "{methods:?}");
        assert!(matches!(methods[0], UnsubMethod::OneClick { .. }));
        assert!(matches!(methods[1], UnsubMethod::Mailto { .. }));
        assert!(matches!(methods[2], UnsubMethod::ManualLink { .. }));
    }

    #[test]
    fn the_first_route_matches_the_single_chosen_one() {
        // methods() must not disagree with method, or the interface would
        // promise one thing and the executor attempt another.
        for (lu, post) in [
            ("<https://a.example/u>", Some("List-Unsubscribe=One-Click")),
            ("<https://a.example/u>", None),
            ("<mailto:a@b.example>", None),
            ("<mailto:a@b.example>, <https://a.example/u>", None),
            ("junk", None),
        ] {
            let p = parse_unsubscribe(Some(lu), post);
            match p.methods().first() {
                Some(first) => assert_eq!(*first, p.method, "for {lu:?}"),
                None => assert_eq!(p.method, UnsubMethod::None, "for {lu:?}"),
            }
        }
    }

    #[test]
    fn a_link_only_sender_offers_exactly_one_route() {
        let p = parse_unsubscribe(Some("<https://a.example/u>"), None);
        assert_eq!(
            p.methods(),
            vec![UnsubMethod::ManualLink {
                url: "https://a.example/u".into()
            }]
        );
    }

    #[test]
    fn absent_header_is_never_actionable() {
        let p = parse_unsubscribe(None, Some("List-Unsubscribe=One-Click"));
        assert!(p.targets.is_empty());
        assert_eq!(p.method, UnsubMethod::None);
        assert!(!p.method.is_actionable());
    }

    // --- Samples modelled on headers seen in the wild -----------------------

    #[test]
    fn real_world_shapes() {
        let cases: Vec<(&str, Option<&str>, bool)> = vec![
            (
                "<https://click.e.example.com/u/?qs=abc123def456>, <mailto:unsub-abc@bounce.example.com>",
                Some("List-Unsubscribe=One-Click"),
                true,
            ),
            (
                "<mailto:unsubscribe-en-abc123@lists.example.org?subject=unsubscribe>",
                None,
                false,
            ),
            (
                "<https://example.com/preferences?id=1&hash=deadbeef>",
                None,
                false,
            ),
            (
                "<https://a.example.com/u/1>,<https://b.example.com/u/2>",
                Some("List-Unsubscribe=One-Click"),
                true,
            ),
        ];
        for (lu, post, expect_one_click) in cases {
            let p = parse_unsubscribe(Some(lu), post);
            assert!(!p.targets.is_empty(), "no targets for {lu:?}");
            assert_eq!(
                matches!(p.method, UnsubMethod::OneClick { .. }),
                expect_one_click,
                "one-click mismatch for {lu:?}"
            );
        }
    }

    #[test]
    fn first_https_wins_when_several_are_offered() {
        let p = parse_unsubscribe(
            Some("<https://a.example.com/1>, <https://b.example.com/2>"),
            Some("List-Unsubscribe=One-Click"),
        );
        assert_eq!(
            p.method,
            UnsubMethod::OneClick {
                url: "https://a.example.com/1".into()
            }
        );
    }

    #[test]
    fn mailto_with_multiple_recipients_takes_the_first() {
        let p = parse_unsubscribe(Some("<mailto:a@example.com,b@example.com>"), None);
        assert_eq!(p.targets, vec![mailto("a@example.com")]);
    }
}
