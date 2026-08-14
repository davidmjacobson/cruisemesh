//! Which spans of a message body are links, and where they go.
//!
//! Both shells must agree *exactly* on this. A regex written twice drifts,
//! and the drift here is not cosmetic: the whole point of the rule is that a
//! hostile sender cannot get a tappable `javascript:` / `data:` / `file:`
//! payload, or a link whose visible text says one place and whose
//! destination is another. So the rule lives in one place and both shells
//! render whatever this returns.
//!
//! # What counts as a link
//!
//! Exactly two schemes, spelled with `://`:
//!
//! * `https://` — the web, including the Shore Pass and friend-card links
//!   the product itself tells people to send each other.
//! * `cruisemesh://` — the in-app deep link ([`crate::DeepLinkRoute`]).
//!
//! Everything else stays plain text. There is no `http://` (no downgrade to
//! cleartext from a tap), no `mailto:`, no scheme-relative `//host`, no bare
//! domain, and no guessing: `cruisemesh.app` written on its own is prose,
//! not a link, because guessing a scheme is how "text that merely resembles
//! a URL" gets mangled.
//!
//! # Offsets are UTF-16 code units
//!
//! [`CoreDetectedLink::start_utf16`] / [`CoreDetectedLink::end_utf16`] are
//! **UTF-16 code-unit offsets**, half-open, over the same body string the
//! shell already has. That is the one unit both shells index natively:
//! Kotlin `String`/`AnnotatedString` offsets are UTF-16 chars, and Swift's
//! `String.Index` is reachable from a UTF-16 offset via `utf16` +
//! `String.Index(_:within:)`. Rust byte offsets are neither, and a `String`
//! index taken as a byte offset lands mid-character on the first emoji in a
//! message — which is a crash, not a rendering glitch. Emitting the offsets
//! in the shells' own unit means neither shell has to convert, so neither
//! shell can convert wrongly.
//!
//! Astral characters (emoji, most flags) are two UTF-16 units; a body of
//! `"🎉 https://x.com"` starts its link at 3, not 2 and not 5.
//!
//! # Displayed text is the destination, always
//!
//! [`CoreDetectedLink::url`] is byte-for-byte the substring of the body the
//! range covers. Nothing is normalised, lower-cased, re-encoded or
//! completed. **A shell must render the original text and use `url` as the
//! destination unchanged** — that is what makes display-text spoofing
//! structurally impossible here rather than a thing we have to police. It is
//! also why this returns ranges instead of a rewritten string.
//!
//! # What a hostile sender can and cannot do
//!
//! Decisions, all deliberate:
//!
//! * **Embedded credentials are refused outright.** `https://evil.com@real.com`
//!   is not linkified at all. The text reads as `evil.com` and resolves to
//!   `real.com`; there is no honest way to render that, so it stays prose.
//! * **A link stops at the first non-ASCII character, and if that character
//!   could read as part of it, there is no link at all.** A URL containing
//!   Cyrillic `а`, a right-to-left override (U+202E), a zero-width joiner, a
//!   soft hyphen, a variation selector, a combining mark, or a lookalike of
//!   the `.` `/` `:` `@` `-` that a web address is built from, simply is not
//!   a link — not even the ASCII prefix of one, because a tappable prefix
//!   inside a longer address-looking string is exactly the confusion we are
//!   trying to avoid (see `terminator_is_unsafe`, `is_invisible` and
//!   `reads_as_address_syntax`). The same characters are refused
//!   *immediately in front of* a link, because a bidi override there
//!   reverses how the link's own characters are drawn. This does not
//!   *solve* homograph attacks —
//!   punycode (`https://xn--80ak6aa92e.com`) is plain ASCII and still
//!   linkifies — but it cannot make them worse, and it removes the whole
//!   class where the rendered glyphs differ from the characters. Displaying
//!   punycode as punycode is the honest rendering.
//! * **Percent-encoding is allowed in the path, query and fragment and
//!   forbidden in the host.** `%` is not a legal host character here, so
//!   `https://%65vil.com` stays prose while
//!   `https://cruisemesh.app/r/#a%2Fb` links and survives intact.
//! * **Hosts are bounded**: 253 bytes total, 63 per label, at least one dot,
//!   no empty labels, no leading/trailing `-`, `[a-zA-Z0-9.-]` only, plus an
//!   optional numeric port. IPv6 literals (`https://[::1]/`) are refused —
//!   the brackets fight the trailing-punctuation rule and no family member
//!   pastes one.
//! * **Whole links are bounded** at 2048 bytes, so a mile-long host cannot
//!   turn one message into an unreadable underline.
//! * **A link cannot start mid-token.** If the character before the scheme is
//!   an ASCII alphanumeric or one of `+-.:/@_~%`, the match is skipped, so
//!   `javascript:https://evil.example`, `//https://x.com` and
//!   `nothttps://x.com` produce no link.
//!
//! The `cruisemesh://` form is deliberately narrower: the authority must be
//! a short alphanumeric route word (`f`, `r`, `lan` today, see
//! [`crate::deep_link_route`]). Spellings we do not serve stay plain text.

/// Which of the two allowed schemes a detected link uses. The shells route
/// on this: `Https` leaves the app (confirm first), `CruiseMesh` is handled
/// in-app via [`crate::deep_link_route`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreLinkScheme {
    /// `https://` — a web address.
    Https,
    /// `cruisemesh://` — an in-app destination.
    CruiseMesh,
}

/// One link found in a message body.
///
/// The range is half-open in **UTF-16 code units** over the body that was
/// passed in, so a shell can style `start_utf16..end_utf16` directly. `url`
/// is exactly that substring; render the substring, open `url`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreDetectedLink {
    /// First UTF-16 code unit of the link, inclusive.
    pub start_utf16: u32,
    /// One past the last UTF-16 code unit of the link, exclusive.
    pub end_utf16: u32,
    /// The link text, byte-for-byte as it appears in the body. This is both
    /// what must be displayed and where the tap must go.
    pub url: String,
    /// Which scheme it uses.
    pub scheme: CoreLinkScheme,
}

const HTTPS_PREFIX: &str = "https://";
const APP_PREFIX: &str = "cruisemesh://";

/// Longest link we will mark up. Longer candidates stay plain text.
///
/// Measured over the unbroken run of URL characters, *before* trailing
/// punctuation is handed back — so the cap also bounds the work any one
/// candidate can cost, whatever the sender pads it with.
const MAX_LINK_BYTES: usize = 2048;
/// DNS limits, so an absurd host cannot swallow a message.
const MAX_HOST_BYTES: usize = 253;
const MAX_LABEL_BYTES: usize = 63;
/// `cruisemesh://<route>` — route words are short by construction.
const MAX_APP_ROUTE_BYTES: usize = 32;

/// Find every link in `body`, in order, non-overlapping.
///
/// Returns an empty list for a body with no links (including an empty body).
/// See the module docs for the scheme allow-list, the UTF-16 offset
/// contract, and what is deliberately refused.
#[uniffi::export]
pub fn core_detect_links(body: String) -> Vec<CoreDetectedLink> {
    let text = body.as_str();
    let mut spans: Vec<(usize, usize, CoreLinkScheme)> = Vec::new();
    let mut search_from = 0usize;

    while search_from < text.len() {
        let Some((start, scheme)) = next_candidate(text, search_from) else {
            break;
        };
        let after_scheme = start + prefix_for(scheme).len();

        // A link may not begin in the middle of another token.
        if !starts_cleanly(text, start) {
            search_from = after_scheme;
            continue;
        }

        let Scan::Ended {
            end: scanned_end,
            terminator,
        } = scan_url_end(
            text,
            after_scheme,
            MAX_LINK_BYTES - prefix_for(scheme).len(),
        )
        else {
            // Over the length cap: not a link, and not worth scanning
            // further from here.
            search_from = after_scheme;
            continue;
        };

        // A link that was cut short by a character which reads as part of it
        // is not a link at all: see `terminator_is_unsafe`.
        if terminator.is_some_and(terminator_is_unsafe) {
            search_from = after_scheme;
            continue;
        }

        let end = trim_trailing_punctuation(text, after_scheme, scanned_end);

        if end > after_scheme && authority_is_acceptable(&text[after_scheme..end], scheme) {
            spans.push((start, end, scheme));
            search_from = end;
        } else {
            search_from = after_scheme;
        }
    }

    // One left-to-right pass converts byte offsets to UTF-16 offsets; the
    // spans are already sorted and disjoint, so the cursor only moves
    // forward.
    let mut cursor = (0usize, 0u32);
    spans
        .into_iter()
        .map(|(start, end, scheme)| {
            let start_utf16 = advance_utf16(text, &mut cursor, start);
            let end_utf16 = advance_utf16(text, &mut cursor, end);
            CoreDetectedLink {
                start_utf16,
                end_utf16,
                url: text[start..end].to_string(),
                scheme,
            }
        })
        .collect()
}

/// Is this exact string, whole and entire, a link we are willing to open?
///
/// Shells call this at tap time, when all they hold is the destination
/// string, to decide between "leave the app" and "route in-app" — and to
/// refuse anything that is not one of the two allowed schemes even if it
/// somehow reached them. Trailing punctuation, surrounding whitespace or
/// any trailing text make it `None`, because then the string is not itself
/// a link.
#[uniffi::export]
pub fn core_link_openable_scheme(url: String) -> Option<CoreLinkScheme> {
    let links = core_detect_links(url.clone());
    match links.as_slice() {
        [only] if only.url == url => Some(only.scheme),
        _ => None,
    }
}

fn prefix_for(scheme: CoreLinkScheme) -> &'static str {
    match scheme {
        CoreLinkScheme::Https => HTTPS_PREFIX,
        CoreLinkScheme::CruiseMesh => APP_PREFIX,
    }
}

/// Next `https://` or `cruisemesh://` at or after `from`, matched
/// case-insensitively and only on a character boundary.
fn next_candidate(text: &str, from: usize) -> Option<(usize, CoreLinkScheme)> {
    text[from..].char_indices().find_map(|(offset, _)| {
        let index = from + offset;
        let rest = &text[index..];
        if starts_with_ignore_case(rest, HTTPS_PREFIX) {
            Some((index, CoreLinkScheme::Https))
        } else if starts_with_ignore_case(rest, APP_PREFIX) {
            Some((index, CoreLinkScheme::CruiseMesh))
        } else {
            None
        }
    })
}

fn starts_with_ignore_case(haystack: &str, ascii_needle: &str) -> bool {
    haystack.len() >= ascii_needle.len()
        && haystack.as_bytes()[..ascii_needle.len()].eq_ignore_ascii_case(ascii_needle.as_bytes())
}

/// A link may not start immediately after a character that would make it
/// part of a larger token — another scheme's colon, a slash, an `@`, or any
/// word character. Ordinary non-ASCII neighbours (CJK, punctuation, emoji)
/// are fine.
///
/// The last two clauses mirror [`terminator_is_unsafe`] at the other end of
/// the link. A character that draws nothing — a bidi override above all —
/// sitting immediately in front of a link changes how the link's *own*
/// characters are laid out, so the underlined run stops being the
/// destination; and a character that reads as `.`, `/`, `:` or `@` puts the
/// start of the underline inside something that reads as a longer address,
/// which is the same confusion `.` and `/` are already refused for.
fn starts_cleanly(text: &str, start: usize) -> bool {
    match text[..start].chars().next_back() {
        None => true,
        Some(previous) => {
            !previous.is_ascii_alphanumeric()
                && !matches!(
                    previous,
                    '+' | '-' | '.' | ':' | '/' | '@' | '_' | '~' | '%'
                )
                && !is_invisible(previous)
                && !reads_as_address_syntax(previous)
        }
    }
}

/// True for the ASCII characters RFC 3986 allows in a URI. Anything else —
/// whitespace, control bytes, `"`, `<`, `>`, `\`, `{`, `}`, `|`, backtick,
/// and every non-ASCII character — ends the link.
fn is_url_char(character: char) -> bool {
    if !character.is_ascii() {
        return false;
    }
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '-' | '.'
                | '_'
                | '~'
                | '%'
                | ':'
                | '/'
                | '?'
                | '#'
                | '['
                | ']'
                | '@'
                | '!'
                | '$'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | ';'
                | '='
        )
}

/// How the forward scan from the end of the scheme finished.
enum Scan {
    /// The URL body runs to `end`; `terminator` is the character that
    /// stopped the scan (`None` at end of body). *What* stopped it decides
    /// whether the candidate is trustworthy at all.
    Ended {
        end: usize,
        terminator: Option<char>,
    },
    /// The run of URL characters went past the length cap. Refused, and the
    /// scan stops there rather than reading to the end of the body — so a
    /// message that is nothing but URL characters cannot cost more than
    /// [`MAX_LINK_BYTES`] per candidate.
    TooLong,
}

fn scan_url_end(text: &str, from: usize, max_bytes: usize) -> Scan {
    let mut end = from;
    for character in text[from..].chars() {
        if !is_url_char(character) {
            return Scan::Ended {
                end,
                terminator: Some(character),
            };
        }
        if end - from + character.len_utf8() > max_bytes {
            return Scan::TooLong;
        }
        end += character.len_utf8();
    }
    Scan::Ended {
        end,
        terminator: None,
    }
}

/// A terminator that a reader will not perceive as ending the link.
///
/// `https://real.example.cоm/pay` (Cyrillic `о`) would otherwise scan as the
/// valid host `real.example.c` and be linkified, leaving a tappable prefix
/// inside a longer string that reads as a different address. The underline
/// boundary is far too subtle to be the only warning, so the whole candidate
/// is dropped and the text stays prose.
///
/// The same applies to invisible characters — bidi overrides (U+202A‥U+202E,
/// U+2066‥U+2069), zero-width and BOM characters, and combining marks, all
/// of which can change what the surrounding text looks like without changing
/// what it says.
///
/// Terminators that genuinely read as a break — ASCII whitespace and
/// punctuation, CJK punctuation, emoji and other symbols — are fine, so
/// `見て https://x.example。` and `https://x.example🎉` still link.
///
/// # Known residual
///
/// Combining marks are covered by `Alphabetic` plus the blocks listed below,
/// which between them catch every mark a Latin, Greek, Cyrillic, Hebrew,
/// Arabic, Thai or Japanese sender can type. The script-specific marks that
/// are neither — a Devanagari nukta (U+093C), a Thai tone mark (U+0E48) —
/// still end a link. They are *visible*: they draw a stroke on the last
/// character of the link rather than hiding the boundary, so they shorten a
/// link rather than disguise one, and the confirmation dialog still shows the
/// destination in full. Closing the gap properly needs a Unicode mark table,
/// which is a dependency decision for the whole core rather than part of this
/// rule. Note the two shells differ here: such a span ends inside a grapheme
/// cluster, so iOS drops the link and Android draws it.
fn terminator_is_unsafe(character: char) -> bool {
    // `is_alphanumeric` is the Unicode `Alphabetic` property, so it already
    // covers the combining marks that are alphabetic in their own right
    // (Hebrew points, Thai vowel signs, Arabic marks). The blocks below are
    // the ones it does not.
    character.is_alphanumeric()
        || is_invisible(character)
        || reads_as_address_syntax(character)
        || matches!(character,
            '\u{0300}'..='\u{036f}'
                | '\u{1ab0}'..='\u{1aff}'
                | '\u{1dc0}'..='\u{1dff}'
                | '\u{20d0}'..='\u{20f0}'
                | '\u{3099}'..='\u{309a}'
                | '\u{fe20}'..='\u{fe2f}')
}

/// Characters that draw nothing at all.
///
/// A link that stops in front of one *looks* like it runs straight into
/// whatever follows: `https://evil.example\u{ad}.apple.com` renders as
/// `https://evil.example.apple.com`, reads as a page on `apple.com`, and taps
/// through to `evil.example`. That is the same attack as the Cyrillic `о`
/// above, so it gets the same answer — no link at all.
///
/// Control bytes count. The whitespace ones do not, because a tab or a
/// newline genuinely does end the line, and a link at the end of a line is
/// the ordinary case.
fn is_invisible(character: char) -> bool {
    (character.is_control() && !character.is_whitespace())
        || matches!(character,
            '\u{00ad}'                    // soft hyphen
                | '\u{061c}'              // Arabic letter mark
                | '\u{180b}'..='\u{180f}' // Mongolian selectors, vowel separator
                | '\u{200b}'..='\u{200f}' // zero widths, LRM/RLM
                | '\u{202a}'..='\u{202e}' // bidi embeddings and overrides
                | '\u{2060}'..='\u{2065}' // word joiner, invisible operators
                | '\u{2066}'..='\u{2069}' // bidi isolates
                | '\u{fe00}'..='\u{fe0f}' // variation selectors
                | '\u{feff}'              // byte order mark
                | '\u{fff9}'..='\u{fffb}' // interlinear annotation
                | '\u{e0000}'..='\u{e0fff}') // tag characters, variation supplement
}

/// Characters that read as one of the four separators a web address is built
/// from — `.`, `/`, `:`, `@` — plus the `-` that joins labels inside a host,
/// without actually being them.
///
/// `https://evil.example\u{2024}apple.com` uses a one-dot leader, which draws
/// as an ordinary full stop at message size. The scan stops there, `evil.example`
/// is a perfectly good host, and the result would be a tappable
/// `https://evil.example` sitting inside text that reads as `apple.com`.
///
/// Deliberately narrow: only the separators that let the *tail* read as more
/// of the same address. Fullwidth sentence punctuation (`！`, `？`, `）`) and
/// the ideographic full stop in both its widths (`。`, `｡` — a small circle,
/// not a dot) are ordinary ways to end a sentence in Japanese and stay safe;
/// a fullwidth `？` would in any case only make the tail read as a query
/// string on a host that is still plainly visible.
fn reads_as_address_syntax(character: char) -> bool {
    matches!(
        character,
        // Full stops.
        '\u{2024}' | '\u{2025}' | '\u{fe52}' | '\u{ff0e}'
        // Solidi.
        | '\u{2044}' | '\u{2215}' | '\u{29f8}' | '\u{ff0f}'
        // Colons.
        | '\u{2236}' | '\u{a789}' | '\u{fe55}' | '\u{ff1a}'
        // At signs.
        | '\u{fe6b}' | '\u{ff20}'
        // Hyphens. Not the en and em dashes: they are visibly longer than a
        // hyphen and they are how people punctuate a sentence.
        | '\u{2010}'..='\u{2012}' | '\u{2212}' | '\u{fe58}' | '\u{fe63}' | '\u{ff0d}'
    )
}

/// Give back sentence punctuation that the greedy scan swallowed.
///
/// `.,:;!?'` never end a link people actually paste, and closing brackets
/// only belong to the link when the link opened them — so
/// `visit https://x.com.` keeps its full stop as prose, `(https://x.com)`
/// keeps its parentheses, and
/// `https://en.wikipedia.org/wiki/Cruise_(disambiguation)` keeps its.
fn trim_trailing_punctuation(text: &str, start: usize, mut end: usize) -> usize {
    // Counted once; only closers are ever trimmed, so the opener counts
    // stay valid as the end moves left.
    let slice = &text[start..end];
    let opened_round = count(slice, '(');
    let mut closed_round = count(slice, ')');
    let opened_square = count(slice, '[');
    let mut closed_square = count(slice, ']');

    while end > start {
        let Some(last) = text[start..end].chars().next_back() else {
            break;
        };
        let drop = match last {
            '.' | ',' | ':' | ';' | '!' | '?' | '\'' => true,
            ')' if closed_round > opened_round => {
                closed_round -= 1;
                true
            }
            ']' if closed_square > opened_square => {
                closed_square -= 1;
                true
            }
            _ => false,
        };
        if !drop {
            break;
        }
        end -= last.len_utf8();
    }
    end
}

fn count(text: &str, needle: char) -> usize {
    text.chars()
        .filter(|character| *character == needle)
        .count()
}

/// Everything after the scheme is `authority [ "/" path ] [ "?" query ]
/// [ "#" fragment ]`; only the authority is policed.
fn authority_is_acceptable(after_scheme: &str, scheme: CoreLinkScheme) -> bool {
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() {
        return false;
    }
    // `https://evil.com@real.com` reads as one place and goes to another.
    if authority.contains('@') {
        return false;
    }
    match scheme {
        CoreLinkScheme::Https => host_is_acceptable(authority),
        CoreLinkScheme::CruiseMesh => {
            authority.len() <= MAX_APP_ROUTE_BYTES
                && authority
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        }
    }
}

fn host_is_acceptable(authority: &str) -> bool {
    // IPv6 literals: refused, see the module docs.
    if authority.starts_with('[') {
        return false;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if let Some(port) = port {
        if port.is_empty()
            || port.len() > 5
            || !port.chars().all(|character| character.is_ascii_digit())
        {
            return false;
        }
    }
    if host.is_empty() || host.len() > MAX_HOST_BYTES || !host.contains('.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= MAX_LABEL_BYTES
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
    })
}

/// Move a `(byte, utf16)` cursor forward to `to_byte` (a character boundary
/// at or after the cursor) and report the UTF-16 offset there.
fn advance_utf16(text: &str, cursor: &mut (usize, u32), to_byte: usize) -> u32 {
    for character in text[cursor.0..to_byte].chars() {
        cursor.1 += character.len_utf16() as u32;
    }
    cursor.0 = to_byte;
    cursor.1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spans a body yields, as `(start_utf16, end_utf16, url, scheme)`.
    fn detect(body: &str) -> Vec<(u32, u32, String, CoreLinkScheme)> {
        core_detect_links(body.to_string())
            .into_iter()
            .map(|link| (link.start_utf16, link.end_utf16, link.url, link.scheme))
            .collect()
    }

    fn urls(body: &str) -> Vec<String> {
        core_detect_links(body.to_string())
            .into_iter()
            .map(|link| link.url)
            .collect()
    }

    /// Every span must be exactly the UTF-16 slice it claims — this is the
    /// property the shells rely on, so assert it everywhere.
    fn assert_spans_match_text(body: &str) {
        let units: Vec<u16> = body.encode_utf16().collect();
        for link in core_detect_links(body.to_string()) {
            let slice = &units[link.start_utf16 as usize..link.end_utf16 as usize];
            assert_eq!(
                String::from_utf16(slice).expect("span is whole characters"),
                link.url,
                "span does not match its url in {body:?}"
            );
        }
    }

    #[test]
    fn an_empty_body_has_no_links() {
        assert!(detect("").is_empty());
        assert!(detect("   ").is_empty());
    }

    #[test]
    fn prose_with_no_scheme_is_left_alone() {
        // Bare domains are prose. We never guess a scheme.
        for body in [
            "cruisemesh.app",
            "see cruisemesh.app for details",
            "www.example.com",
            "Meet at 3. Bring the charger.",
            "ratio was 4:3 and the file is notes.txt",
        ] {
            assert!(detect(body).is_empty(), "should not linkify {body:?}");
        }
    }

    #[test]
    fn a_plain_https_link_is_found() {
        assert_eq!(
            detect("https://cruisemesh.app"),
            vec![(
                0,
                22,
                "https://cruisemesh.app".to_string(),
                CoreLinkScheme::Https
            )]
        );
    }

    #[test]
    fn a_link_at_position_zero_and_a_link_at_the_very_end() {
        let body = "https://a.example ok https://b.example";
        assert_eq!(
            detect(body),
            vec![
                (
                    0,
                    17,
                    "https://a.example".to_string(),
                    CoreLinkScheme::Https
                ),
                (
                    21,
                    38,
                    "https://b.example".to_string(),
                    CoreLinkScheme::Https
                ),
            ]
        );
        assert_eq!(body.encode_utf16().count(), 38);
        assert_spans_match_text(body);
    }

    #[test]
    fn a_link_inside_a_sentence_keeps_the_sentence_punctuation() {
        assert_eq!(urls("visit https://x.example."), ["https://x.example"]);
        assert_eq!(
            urls("visit https://x.example, then go"),
            ["https://x.example"]
        );
        assert_eq!(urls("really? https://x.example?"), ["https://x.example"]);
        assert_eq!(urls("wow https://x.example!"), ["https://x.example"]);
        assert_eq!(urls("here: https://x.example;"), ["https://x.example"]);
        assert_eq!(urls("it's https://x.example'"), ["https://x.example"]);
        assert_eq!(urls("https://x.example..."), ["https://x.example"]);
    }

    #[test]
    fn a_query_or_path_that_genuinely_ends_in_punctuation_is_not_over_trimmed() {
        assert_eq!(
            urls("https://x.example/a/b?c=1&d=2"),
            ["https://x.example/a/b?c=1&d=2"]
        );
        assert_eq!(
            urls("https://x.example/a_b-c~d"),
            ["https://x.example/a_b-c~d"]
        );
    }

    #[test]
    fn parenthesised_links_give_the_brackets_back() {
        assert_eq!(urls("(https://x.example)"), ["https://x.example"]);
        assert_eq!(urls("[https://x.example]"), ["https://x.example"]);
        assert_eq!(urls("(see https://x.example, ok?)"), ["https://x.example"]);
    }

    #[test]
    fn brackets_the_link_itself_opened_are_kept() {
        assert_eq!(
            urls("https://en.wikipedia.org/wiki/Cruise_(disambiguation)"),
            ["https://en.wikipedia.org/wiki/Cruise_(disambiguation)"]
        );
        assert_eq!(
            urls("(https://en.wikipedia.org/wiki/Cruise_(disambiguation))"),
            ["https://en.wikipedia.org/wiki/Cruise_(disambiguation)"]
        );
    }

    #[test]
    fn the_relay_link_survives_with_its_fragment_intact() {
        let body = "Set up your pass: https://cruisemesh.app/r/#CMRELAY1:eyJ2IjoxfQ";
        assert_eq!(
            urls(body),
            ["https://cruisemesh.app/r/#CMRELAY1:eyJ2IjoxfQ"]
        );
        assert_spans_match_text(body);

        // ...and with a full stop after it, the card must not lose a byte.
        assert_eq!(
            urls("https://cruisemesh.app/r/#CMRELAY1:eyJ2IjoxfQ."),
            ["https://cruisemesh.app/r/#CMRELAY1:eyJ2IjoxfQ"]
        );
        // base64url alphabet includes - and _, which must survive.
        assert_eq!(
            urls("https://cruisemesh.app/f#CMFRIEND2:aB-_cD"),
            ["https://cruisemesh.app/f#CMFRIEND2:aB-_cD"]
        );
        // The compact v3 card form is just another fragment: same treatment.
        assert_eq!(
            urls("https://cruisemesh.app/f#CMFRIEND3:aB-_cD"),
            ["https://cruisemesh.app/f#CMFRIEND3:aB-_cD"]
        );
        assert_spans_match_text("Add me: https://cruisemesh.app/f#CMFRIEND3:aB-_cD thanks");
        // v4 is the same URL shape; link detection must not drop the fragment.
        assert_eq!(
            urls("https://cruisemesh.app/f#CMFRIEND4:aB-_cD"),
            ["https://cruisemesh.app/f#CMFRIEND4:aB-_cD"]
        );
    }

    #[test]
    fn the_app_scheme_is_a_link() {
        assert_eq!(
            detect("cruisemesh://r#CMRELAY1:aBc"),
            vec![(
                0,
                27,
                "cruisemesh://r#CMRELAY1:aBc".to_string(),
                CoreLinkScheme::CruiseMesh
            )]
        );
        assert_eq!(urls("cruisemesh://f#CARD"), ["cruisemesh://f#CARD"]);
        assert_eq!(urls("cruisemesh://lan"), ["cruisemesh://lan"]);
    }

    #[test]
    fn app_scheme_spellings_we_do_not_serve_stay_plain() {
        // No `//`.
        assert!(detect("cruisemesh:f#CARD").is_empty());
        // Empty route.
        assert!(detect("cruisemesh:///f").is_empty());
        // Non-word route.
        assert!(detect("cruisemesh://a.b").is_empty());
        assert!(detect("cruisemesh://evil@f").is_empty());
    }

    #[test]
    fn dangerous_schemes_are_never_links() {
        for body in [
            "javascript:alert(1)",
            "javascript:void(0)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "file:///etc/passwd",
            "mailto:someone@example.com",
            "http://example.com",
            "HTTP://example.com",
            "ftp://example.com/x",
            "tel:+15551234567",
            "//example.com/path",
            "intent://scan#Intent;scheme=zxing;end",
        ] {
            assert!(detect(body).is_empty(), "should not linkify {body:?}");
        }
    }

    #[test]
    fn a_dangerous_scheme_cannot_smuggle_an_allowed_one_in_its_payload() {
        // The inner https:// is preceded by ':' or '/', so it cannot start a
        // link — otherwise we would underline part of a javascript: URL and
        // make it look trustworthy.
        assert!(detect("javascript:https://x.example").is_empty());
        assert!(detect("//https://x.example").is_empty());
        assert!(detect("nothttps://x.example").is_empty());
        assert!(detect("xhttps://x.example").is_empty());
        assert!(detect("bob@https://x.example").is_empty());
        assert!(detect("-https://x.example").is_empty());
    }

    #[test]
    fn embedded_credentials_are_refused_entirely() {
        for body in [
            "https://evil.example@real.example",
            "https://evil.example@real.example/path",
            "https://user:pass@real.example",
            "cruisemesh://evil.example@f",
        ] {
            assert!(detect(body).is_empty(), "should not linkify {body:?}");
        }
    }

    #[test]
    fn percent_encoding_is_allowed_in_the_path_but_not_the_host() {
        assert_eq!(
            urls("https://cruisemesh.app/r/#a%2Fb%20c"),
            ["https://cruisemesh.app/r/#a%2Fb%20c"]
        );
        assert!(detect("https://%65vil.example").is_empty());
        assert!(detect("https://ex%2Eample.com").is_empty());
    }

    #[test]
    fn malformed_or_oversized_hosts_stay_plain() {
        assert!(detect("https://").is_empty());
        assert!(detect("https:// x.example").is_empty());
        assert!(detect("https://localhost").is_empty(), "no dot, no link");
        assert!(detect("https://.example").is_empty());
        assert!(detect("https://example..com").is_empty());
        assert!(detect("https://-bad.example").is_empty());
        assert!(detect("https://bad-.example").is_empty());
        assert!(detect("https://[::1]/x").is_empty());
        assert!(detect("https://x.example:notaport/").is_empty());
        assert!(detect("https://x.example:1234567/").is_empty());

        let long_label = "a".repeat(64);
        assert!(detect(&format!("https://{long_label}.example")).is_empty());

        let long_host = format!("{}.example", "a.".repeat(130));
        assert!(detect(&format!("https://{long_host}")).is_empty());

        let long_path = "b".repeat(MAX_LINK_BYTES);
        assert!(detect(&format!("https://x.example/{long_path}")).is_empty());
    }

    #[test]
    fn the_length_cap_is_exact() {
        let prefix = "https://x.example/";
        let at_cap = format!("{prefix}{}", "b".repeat(MAX_LINK_BYTES - prefix.len()));
        assert_eq!(at_cap.len(), MAX_LINK_BYTES);
        assert_eq!(urls(&at_cap), [at_cap.as_str()]);

        let over_cap = format!("{at_cap}b");
        assert!(detect(&over_cap).is_empty());
    }

    #[test]
    fn adversarial_bodies_stay_cheap_and_produce_nothing() {
        // Every candidate is refused, and none of them may scan the whole
        // body: the work stays linear rather than quadratic.
        let repeated_schemes = "https://".repeat(4000);
        assert!(detect(&repeated_schemes).is_empty());

        // A wall of closing brackets after a link: trimming counts once.
        let brackets = ")".repeat(500);
        assert_eq!(
            urls(&format!("https://x.example{brackets}")),
            ["https://x.example"]
        );

        // A wall of sentence punctuation.
        let dots = ".".repeat(1000);
        assert_eq!(
            urls(&format!("https://x.example{dots}")),
            ["https://x.example"]
        );

        // Nothing but punctuation trims away to an empty authority.
        assert!(detect(&format!("https://{dots}")).is_empty());
    }

    #[test]
    fn ports_and_ip_hosts_are_fine() {
        assert_eq!(
            urls("https://x.example:8443/a"),
            ["https://x.example:8443/a"]
        );
        assert_eq!(
            urls("https://192.168.1.5:8080/"),
            ["https://192.168.1.5:8080/"]
        );
    }

    #[test]
    fn a_confusable_or_invisible_character_kills_the_whole_link() {
        // Cyrillic а right after the scheme: no host at all.
        assert!(detect("https://аpple.com").is_empty(), "cyrillic a");
        // Cyrillic а mid-host: the ASCII prefix `https://x.ex` is a valid
        // host, but linkifying it would leave a tappable prefix inside a
        // string that reads as a different address.
        assert!(detect("https://x.exаmple/path").is_empty());
        assert!(detect("https://real.example.cоm/pay").is_empty());
        // A right-to-left override cannot be smuggled next to a destination.
        assert!(detect("https://x.example/gpj.\u{202e}txt").is_empty());
        // Zero-width joiner, zero-width space and BOM likewise.
        assert!(detect("https://x.example/a\u{200d}b").is_empty());
        assert!(detect("https://x.example\u{200b}evil").is_empty());
        assert!(detect("https://x.example\u{feff}").is_empty());
        // A combining acute would redraw the last character.
        assert!(detect("https://x.example\u{0301}").is_empty());
    }

    /// A character that draws nothing is not a break a reader can see. Each
    /// of these renders as `https://evil.example.apple.com` — one address,
    /// reading as a page on `apple.com` — while the underline and the tap
    /// would cover only `https://evil.example`.
    #[test]
    fn an_invisible_character_cannot_hide_where_a_link_stops() {
        for (name, hidden) in [
            ("soft hyphen", '\u{00ad}'),
            ("Arabic letter mark", '\u{061c}'),
            ("Mongolian vowel separator", '\u{180e}'),
            ("word joiner", '\u{2060}'),
            ("unassigned default-ignorable", '\u{2065}'),
            ("variation selector 16", '\u{fe0f}'),
            ("variation selector supplement", '\u{e0100}'),
            ("tag character", '\u{e0061}'),
            ("interlinear annotation anchor", '\u{fff9}'),
            ("nul", '\u{0}'),
            ("start of heading", '\u{1}'),
            ("delete", '\u{7f}'),
        ] {
            let body = format!("https://evil.example{hidden}.apple.example");
            assert!(detect(&body).is_empty(), "{name} left a tappable prefix");
        }
    }

    /// A lookalike of the punctuation an address is built from continues the
    /// address to a reader, so it cannot be allowed to end a link either.
    #[test]
    fn a_lookalike_separator_cannot_hide_where_a_link_stops() {
        for (name, lookalike) in [
            ("one dot leader", '\u{2024}'),
            ("small full stop", '\u{fe52}'),
            ("fullwidth full stop", '\u{ff0e}'),
            ("fraction slash", '\u{2044}'),
            ("division slash", '\u{2215}'),
            ("fullwidth solidus", '\u{ff0f}'),
            ("modifier letter colon", '\u{a789}'),
            ("fullwidth colon", '\u{ff1a}'),
            ("small commercial at", '\u{fe6b}'),
            ("fullwidth commercial at", '\u{ff20}'),
        ] {
            let body = format!("https://evil.example{lookalike}apple.example");
            assert!(detect(&body).is_empty(), "{name} left a tappable prefix");
        }
        // A lookalike hyphen joins two labels of one host just as well.
        for lookalike in ['\u{2010}', '\u{2011}', '\u{2012}', '\u{2212}', '\u{ff0d}'] {
            let body = format!("https://a.evil{lookalike}bank.example");
            assert!(detect(&body).is_empty(), "{lookalike:?} left a prefix");
        }
    }

    /// Sentence punctuation stays a break, including the fullwidth forms
    /// Japanese sentences actually end with.
    #[test]
    fn sentence_punctuation_still_ends_a_link_honestly() {
        for ending in [
            '\u{3002}', '\u{ff61}', '\u{ff01}', '\u{ff1f}', '\u{ff09}', '\u{2013}', '\u{2014}',
        ] {
            assert_eq!(
                urls(&format!("https://x.example{ending}")),
                ["https://x.example"],
                "{ending:?} should still end the link"
            );
        }
    }

    /// The mirror image: an invisible character *in front of* a link. A bidi
    /// override there lays the link's own characters out backwards, so the
    /// underlined run is no longer the destination.
    #[test]
    fn an_invisible_character_in_front_of_a_link_kills_it_too() {
        for hidden in [
            '\u{202e}', '\u{2066}', '\u{00ad}', '\u{200b}', '\u{feff}', '\u{1}',
        ] {
            let body = format!("apple.example{hidden}https://evil.example");
            assert!(detect(&body).is_empty(), "{hidden:?} in front of a link");
        }
        // ...and a lookalike separator in front, for the same reason `.` and
        // `/` are already refused there.
        assert!(detect("apple.example\u{ff0f}https://evil.example").is_empty());
        // Ordinary text in front is still fine.
        assert_eq!(urls("見て→https://x.example"), ["https://x.example"]);
        assert_eq!(urls("🎉https://x.example"), ["https://x.example"]);
    }

    /// iOS maps these offsets through `String.Index(_:within:)`, which returns
    /// nil when the offset falls inside a grapheme cluster — the link would
    /// then vanish on iOS while Android still drew it. Every character that
    /// glues onto the one before it must therefore kill the link outright,
    /// not merely shorten it.
    #[test]
    fn a_span_never_ends_inside_a_grapheme_cluster() {
        for extender in [
            '\u{0301}',  // combining acute
            '\u{3099}',  // katakana voiced sound mark
            '\u{fe0f}',  // variation selector 16
            '\u{e0100}', // variation selector 17
            '\u{200d}',  // zero-width joiner
        ] {
            let body = format!("https://x.example{extender}");
            assert!(
                detect(&body).is_empty(),
                "span ends mid-grapheme before {extender:?}"
            );
        }
    }

    #[test]
    fn a_terminator_that_genuinely_reads_as_a_break_is_fine() {
        // CJK full stop, emoji, and an arrow all end the link honestly.
        assert_eq!(urls("見て https://x.example。"), ["https://x.example"]);
        assert_eq!(urls("https://x.example🎉"), ["https://x.example"]);
        assert_eq!(urls("https://x.example→next"), ["https://x.example"]);
        assert_eq!(urls("https://x.example\u{00a0}next"), ["https://x.example"]);
    }

    #[test]
    fn punycode_still_links_and_is_shown_as_punycode() {
        // We do not solve homographs; we refuse to hide them. Displayed text
        // is the destination, character for character.
        let links = core_detect_links("https://xn--80ak6aa92e.com".to_string());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://xn--80ak6aa92e.com");
    }

    #[test]
    fn offsets_are_utf16_code_units_not_bytes() {
        // 🎉 is one char, two UTF-16 units, four UTF-8 bytes. A shell using
        // byte offsets would land mid-character and crash.
        let body = "🎉 https://x.example 🎉";
        let links = core_detect_links(body.to_string());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].start_utf16, 3);
        assert_eq!(links[0].end_utf16, 20);
        assert_eq!(links[0].url, "https://x.example");
        assert_ne!(links[0].start_utf16 as usize, body.find("https").unwrap());
        assert_spans_match_text(body);
    }

    #[test]
    fn multi_byte_bmp_text_around_a_link_counts_one_unit_per_character() {
        // Accented Latin and CJK are one UTF-16 unit each but 2-3 bytes.
        let body = "café 日本 https://x.example ✅";
        let links = core_detect_links(body.to_string());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].start_utf16, 8);
        assert_eq!(links[0].end_utf16, 25);
        assert_spans_match_text(body);
    }

    #[test]
    fn a_link_may_follow_non_ascii_text_directly() {
        let body = "見て→https://x.example";
        let links = core_detect_links(body.to_string());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].start_utf16, 3);
        assert_spans_match_text(body);
    }

    #[test]
    fn several_links_in_one_message_are_all_found_in_order() {
        let body = "🎉 https://a.example then cruisemesh://r#CARD and (https://b.example).";
        let links = core_detect_links(body.to_string());
        assert_eq!(
            links
                .iter()
                .map(|link| link.url.as_str())
                .collect::<Vec<_>>(),
            [
                "https://a.example",
                "cruisemesh://r#CARD",
                "https://b.example"
            ]
        );
        assert_eq!(
            links.iter().map(|link| link.scheme).collect::<Vec<_>>(),
            [
                CoreLinkScheme::Https,
                CoreLinkScheme::CruiseMesh,
                CoreLinkScheme::Https
            ]
        );
        // Strictly increasing, non-overlapping.
        for pair in links.windows(2) {
            assert!(pair[0].end_utf16 <= pair[1].start_utf16);
        }
        assert_spans_match_text(body);
    }

    #[test]
    fn the_scheme_is_case_insensitive_but_the_text_is_never_rewritten() {
        let links = core_detect_links("HTTPS://X.Example/Path".to_string());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "HTTPS://X.Example/Path");
        let links = core_detect_links("CruiseMesh://F#Card".to_string());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "CruiseMesh://F#Card");
        assert_eq!(links[0].scheme, CoreLinkScheme::CruiseMesh);
    }

    #[test]
    fn newlines_and_tabs_end_a_link() {
        assert_eq!(
            urls("https://a.example\nhttps://b.example"),
            ["https://a.example", "https://b.example"]
        );
        assert_eq!(urls("https://a.example\tx"), ["https://a.example"]);
        assert_eq!(urls("\"https://a.example\""), ["https://a.example"]);
        assert_eq!(urls("<https://a.example>"), ["https://a.example"]);
    }

    #[test]
    fn the_displayed_text_always_equals_the_destination() {
        for body in [
            "https://cruisemesh.app/r/#CMRELAY1:aBc",
            "call me: (https://x.example/a(b)c), ok?",
            "🎉🎉 cruisemesh://f#CARD 🎉🎉",
        ] {
            for link in core_detect_links(body.to_string()) {
                let units: Vec<u16> = body.encode_utf16().collect();
                let shown =
                    String::from_utf16(&units[link.start_utf16 as usize..link.end_utf16 as usize])
                        .expect("span is whole characters");
                assert_eq!(shown, link.url, "display text must equal destination");
            }
        }
    }

    #[test]
    fn openable_scheme_accepts_only_a_whole_allowed_link() {
        assert_eq!(
            core_link_openable_scheme("https://cruisemesh.app/r/#CMRELAY1:aBc".into()),
            Some(CoreLinkScheme::Https)
        );
        assert_eq!(
            core_link_openable_scheme("cruisemesh://f#CARD".into()),
            Some(CoreLinkScheme::CruiseMesh)
        );
        for bad in [
            "",
            "  ",
            "http://x.example",
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///etc/passwd",
            "https://evil.example@real.example",
            "https://x.example.",
            " https://x.example",
            "https://x.example trailing",
            "cruisemesh.app",
        ] {
            assert_eq!(
                core_link_openable_scheme(bad.to_string()),
                None,
                "should not be openable: {bad:?}"
            );
        }
    }
}
