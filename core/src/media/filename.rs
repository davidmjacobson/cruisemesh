//! A manifest filename is display metadata, never a path.
//!
//! The sender picked the string; the receiver writes it to a filesystem. That
//! makes it the one field of the media plane that a peer can aim at the
//! device, so the rule the spec states — "no separators, no traversal, no
//! leading dots, extension derived from the manifest mime rather than trusted
//! from the name" — lives here as pure policy with a table of cases, not in
//! whichever shell writes the file first.
//!
//! Two properties are load-bearing:
//!
//! * The result is always a usable single filename. A name that sanitizes to
//!   nothing still yields `file.<ext>`, because a driver that has to handle
//!   `""` will eventually not.
//! * The extension comes from [`extension_for_mime`] alone. A name claiming
//!   `.exe` under `image/jpeg` is saved as a `.jpg`; the sender does not get
//!   to choose how the platform will treat the bytes.
//!
//! Collision handling is the driver's — only it knows what is already in the
//! document directory — but the suffix *shape* is core's, via
//! [`sanitize_media_filename_with_suffix`], so both shells collide the same
//! way.

/// Longest filename the manifest carries, and so the longest this returns.
pub const MAX_FILENAME_BYTES: usize = 255;

/// Longest extension we will derive from a mime type. Past this the subtype
/// is a vendor string rather than an extension, and `bin` is the honest read.
const MAX_EXTENSION_BYTES: usize = 16;

/// The fallback stem for a name that sanitizes away entirely.
const FALLBACK_STEM: &str = "file";

/// The fallback extension for a mime type with no usable subtype.
const FALLBACK_EXTENSION: &str = "bin";

/// Windows reserved device names. The desktop crate is in this workspace, and
/// a file called `CON.txt` is not a file there.
const RESERVED_STEMS: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Subtypes whose conventional extension is not the subtype itself.
const EXTENSION_OVERRIDES: [(&str, &str); 6] = [
    ("image/jpeg", "jpg"),
    ("image/svg+xml", "svg"),
    ("video/quicktime", "mov"),
    ("text/plain", "txt"),
    ("text/markdown", "md"),
    ("application/octet-stream", "bin"),
];

/// The name a received blob is written under: one filesystem-safe component,
/// extension derived from `mime`, never empty.
///
/// Exported: the receive-side save path is a shell's, and the rule it applies
/// must not be. The collision-suffix form stays unexported until a driver
/// exists to loop it (phase 3).
#[uniffi::export]
pub fn sanitize_media_filename(name: &str, mime: &str) -> String {
    sanitize_media_filename_with_suffix(name, mime, 0)
}

/// The same name with a collision suffix. `suffix == 0` is the unsuffixed
/// name, so a driver can loop from zero without special-casing the first try.
pub fn sanitize_media_filename_with_suffix(name: &str, mime: &str, suffix: u32) -> String {
    let extension = extension_for_mime(mime);
    let marker = if suffix == 0 {
        String::new()
    } else {
        format!(" ({suffix})")
    };

    // The extension and the collision marker are not negotiable, so the stem
    // absorbs the whole length bound.
    let reserved = marker.len() + 1 + extension.len();
    let stem_budget = MAX_FILENAME_BYTES.saturating_sub(reserved);
    let stem = truncate_bytes(&stem_of(name), stem_budget);
    let stem = if stem.is_empty() {
        truncate_bytes(FALLBACK_STEM, stem_budget)
    } else {
        stem
    };

    format!("{stem}{marker}.{extension}")
}

/// The extension a mime type earns. Lowercase, alphanumeric, bounded.
pub fn extension_for_mime(mime: &str) -> String {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if let Some((_, extension)) = EXTENSION_OVERRIDES.iter().find(|(m, _)| *m == mime) {
        return (*extension).to_string();
    }
    let subtype = match mime.split_once('/') {
        Some((_, subtype)) => subtype,
        None => return FALLBACK_EXTENSION.to_string(),
    };
    // `svg+xml` is an svg; `vnd.…` is a vendor string, not an extension.
    let subtype = subtype.split('+').next().unwrap_or("");
    let subtype = subtype.strip_prefix("x-").unwrap_or(subtype);
    if subtype.is_empty()
        || subtype.len() > MAX_EXTENSION_BYTES
        || !subtype.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return FALLBACK_EXTENSION.to_string();
    }
    subtype.to_string()
}

/// Everything before the extension: the last path component, stripped of
/// anything a filesystem would read as structure.
fn stem_of(name: &str) -> String {
    // Traversal dies at the separators: only the last component survives, and
    // `.`/`..` are not components worth keeping.
    let last = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| c.is_whitespace());
    if last == "." || last == ".." {
        return String::new();
    }

    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if is_unsafe(c) { '_' } else { c })
        .collect();

    // The name's own extension is not trusted, so it is not carried: the mime
    // supplies one at the end.
    let cleaned = match cleaned.rsplit_once('.') {
        Some((stem, tail))
            if !stem.is_empty()
                && tail.len() <= MAX_EXTENSION_BYTES
                && tail.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            stem
        }
        _ => cleaned.as_str(),
    };

    // Leading dots make hidden files on unix and confuse every picker;
    // trailing dots and spaces are illegal on Windows.
    let cleaned = cleaned
        .trim_start_matches('.')
        .trim_matches(|c: char| c.is_whitespace() || c == '.');

    if RESERVED_STEMS
        .iter()
        .any(|reserved| cleaned.eq_ignore_ascii_case(reserved))
    {
        return format!("_{cleaned}");
    }
    cleaned.to_string()
}

/// `<`, `>`, `:`, `"`, `|`, `?`, `*` are illegal in a Windows filename, and a
/// NUL truncates a C path on every platform.
fn is_unsafe(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\0')
}

/// Truncate on a char boundary, so a multi-byte name shortens rather than
/// becoming invalid UTF-8.
fn truncate_bytes(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sanitizer_table() {
        // Traversal and separators: only a last component, never a path.
        assert_eq!(
            sanitize_media_filename("..\\..\\etc\\passwd", "application/pdf"),
            "passwd.pdf"
        );
        assert_eq!(sanitize_media_filename("a/b/c.jpg", "image/jpeg"), "c.jpg");
        assert_eq!(sanitize_media_filename("..", "text/plain"), "file.txt");
        assert_eq!(sanitize_media_filename("/", "text/plain"), "file.txt");

        // Leading dots never survive; an empty name still names something.
        assert_eq!(
            sanitize_media_filename(".hidden", "text/plain"),
            "hidden.txt"
        );
        assert_eq!(sanitize_media_filename("", "text/plain"), "file.txt");
        assert_eq!(sanitize_media_filename("...", "text/plain"), "file.txt");

        // The mime decides the extension, not the name.
        assert_eq!(
            sanitize_media_filename("payroll.exe", "image/jpeg"),
            "payroll.jpg"
        );
        assert_eq!(
            sanitize_media_filename("report.pdf", "application/pdf"),
            "report.pdf"
        );

        // Control characters and Windows-illegal characters do not reach a
        // filesystem call.
        assert_eq!(
            sanitize_media_filename("we\u{0}ird\nname", "text/plain"),
            "weirdname.txt"
        );
        assert_eq!(
            sanitize_media_filename("a:b*c?", "text/plain"),
            "a_b_c_.txt"
        );

        // Windows device names are not files.
        assert_eq!(sanitize_media_filename("CON", "text/plain"), "_CON.txt");
        assert_eq!(
            sanitize_media_filename("com1.txt", "text/plain"),
            "_com1.txt"
        );
    }

    #[test]
    fn a_long_name_is_bounded_with_room_for_the_extension() {
        let long = "n".repeat(400);
        let out = sanitize_media_filename(&long, "image/jpeg");
        assert!(out.len() <= MAX_FILENAME_BYTES);
        assert!(out.ends_with(".jpg"));

        // And the bound holds once a collision suffix is charged too.
        let suffixed = sanitize_media_filename_with_suffix(&long, "image/jpeg", 17);
        assert!(suffixed.len() <= MAX_FILENAME_BYTES);
        assert!(suffixed.ends_with(" (17).jpg"));
    }

    #[test]
    fn a_multibyte_name_truncates_on_a_char_boundary() {
        // Naive byte slicing here would panic rather than shorten.
        let long = "é".repeat(300);
        let out = sanitize_media_filename(&long, "text/plain");
        assert!(out.len() <= MAX_FILENAME_BYTES);
        assert!(out.ends_with(".txt"));
    }

    #[test]
    fn the_zero_suffix_is_the_plain_name() {
        assert_eq!(
            sanitize_media_filename_with_suffix("trip.pdf", "application/pdf", 0),
            sanitize_media_filename("trip.pdf", "application/pdf")
        );
        assert_eq!(
            sanitize_media_filename_with_suffix("trip.pdf", "application/pdf", 2),
            "trip (2).pdf"
        );
    }

    #[test]
    fn an_unusable_mime_falls_back_rather_than_inventing_an_extension() {
        assert_eq!(extension_for_mime("application/pdf"), "pdf");
        assert_eq!(extension_for_mime("image/jpeg"), "jpg");
        assert_eq!(extension_for_mime("image/svg+xml"), "svg");
        assert_eq!(extension_for_mime("video/quicktime"), "mov");
        assert_eq!(extension_for_mime("text/plain; charset=utf-8"), "txt");
        assert_eq!(extension_for_mime("application/x-tar"), "tar");
        assert_eq!(
            extension_for_mime(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            ),
            "bin"
        );
        assert_eq!(extension_for_mime("nonsense"), "bin");
        assert_eq!(extension_for_mime(""), "bin");
        assert_eq!(extension_for_mime("application/../../etc"), "bin");
    }
}
