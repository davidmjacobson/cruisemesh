//! Which in-app destination an incoming link addresses.
//!
//! Both shells accept the same links in two forms:
//!
//! * the https Universal / App Link (`https://cruisemesh.app/f#CARD`), which
//!   is what the website and the QR codes carry, and
//! * the `cruisemesh://` scheme (`cruisemesh://f#CARD`), which exists because
//!   **iOS does not fire a Universal Link for a same-domain navigation** — an
//!   "Open in CruiseMesh" button on cruisemesh.app pointing back at
//!   cruisemesh.app is inert in Safari by design, and that is exactly the
//!   button a buyer taps on `/r` after their email opened in a browser
//!   (field-reported 2026-07-27). A custom scheme fires regardless of origin.
//!
//! The routing table itself is one thing both shells must agree on, so it
//! lives here rather than being written twice. Card parsing stays where it
//! already is: the shells hand the fragment to `parse_friend_text` /
//! `parse_relay_setup_text` / `parse_lan_endpoint_link`, which decide whether
//! the payload is real.

/// The in-app destination named by a link, independent of which form the
/// link arrived in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DeepLinkRoute {
    /// A friend card (`/f`) — add-a-friend.
    Friend,
    /// A Shore Pass setup card (`/r`) — internet delivery setup.
    RelaySetup,
    /// A LAN endpoint (`/lan`) — diagnostics-only manual connect.
    Lan,
}

const WEB_HOST: &str = "cruisemesh.app";
const APP_SCHEME: &str = "cruisemesh";

/// Resolve a link's destination from its already-parsed components.
///
/// `host` and `path` are taken as each platform's URL type reports them:
/// `https://cruisemesh.app/f` gives host `cruisemesh.app` and path `/f`,
/// while `cruisemesh://f` gives host `f` and an empty path. Both are
/// accepted, as is the trailing-slash spelling of either. Anything else —
/// including a stray host on the app scheme, or the web host on some other
/// scheme — returns `None`, so an unknown link is ignored rather than
/// guessed at.
#[uniffi::export]
pub fn deep_link_route(scheme: String, host: String, path: String) -> Option<DeepLinkRoute> {
    let scheme = scheme.trim().to_ascii_lowercase();
    let host = host.trim().to_ascii_lowercase();
    let path = path.trim();

    let segment = match scheme.as_str() {
        "https" if host == WEB_HOST => path,
        // `cruisemesh://f` puts the destination in the host; some URL parsers
        // hand back an empty host and `/f` instead, so accept either.
        APP_SCHEME if host.is_empty() => path,
        APP_SCHEME => host.as_str(),
        _ => return None,
    };

    match segment.trim_matches('/') {
        "f" => Some(DeepLinkRoute::Friend),
        "r" => Some(DeepLinkRoute::RelaySetup),
        "lan" => Some(DeepLinkRoute::Lan),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_links_route_as_before() {
        for (path, want) in [
            ("/f", DeepLinkRoute::Friend),
            ("/f/", DeepLinkRoute::Friend),
            ("/r", DeepLinkRoute::RelaySetup),
            ("/r/", DeepLinkRoute::RelaySetup),
            ("/lan", DeepLinkRoute::Lan),
            ("/lan/", DeepLinkRoute::Lan),
        ] {
            assert_eq!(
                deep_link_route("https".into(), WEB_HOST.into(), path.into()),
                Some(want),
                "https path {path}"
            );
        }
    }

    /// Routing looks at scheme/host/path only, so which friend-card form the
    /// fragment carries cannot change where the link lands. Pinned because the
    /// v3 card form ships parser-first and must reach the same screen as v2.
    #[test]
    fn a_friend_link_routes_the_same_whichever_card_form_it_carries() {
        for path in ["/f", "/f/"] {
            assert_eq!(
                deep_link_route("https".into(), WEB_HOST.into(), path.into()),
                Some(DeepLinkRoute::Friend),
            );
        }
        for card in [
            "CMFRIEND1:eyJuYW1lIjoiRGF2ZSJ9",
            "CMFRIEND2:aB-_cD",
            "CMFRIEND3:aB-_cD",
        ] {
            let url = format!("https://{WEB_HOST}/f#{card}");
            let (before_fragment, _) = url.split_once('#').unwrap();
            let path = before_fragment.trim_start_matches(&format!("https://{WEB_HOST}"));
            assert_eq!(
                deep_link_route("https".into(), WEB_HOST.into(), path.into()),
                Some(DeepLinkRoute::Friend),
                "{url}"
            );
        }
    }

    #[test]
    fn app_scheme_routes_from_the_host() {
        for (host, want) in [
            ("f", DeepLinkRoute::Friend),
            ("r", DeepLinkRoute::RelaySetup),
            ("lan", DeepLinkRoute::Lan),
        ] {
            assert_eq!(
                deep_link_route(APP_SCHEME.into(), host.into(), String::new()),
                Some(want),
                "cruisemesh://{host}"
            );
        }
    }

    #[test]
    fn app_scheme_also_accepts_the_destination_in_the_path() {
        assert_eq!(
            deep_link_route(APP_SCHEME.into(), String::new(), "/r".into()),
            Some(DeepLinkRoute::RelaySetup),
        );
    }

    #[test]
    fn scheme_and_host_are_case_insensitive() {
        assert_eq!(
            deep_link_route("HTTPS".into(), "CruiseMesh.App".into(), "/f".into()),
            Some(DeepLinkRoute::Friend),
        );
        assert_eq!(
            deep_link_route("CruiseMesh".into(), "F".into(), String::new()),
            Some(DeepLinkRoute::Friend),
        );
    }

    #[test]
    fn unknown_links_are_ignored() {
        // Right scheme, wrong host: never ours.
        assert_eq!(
            deep_link_route("https".into(), "evil.example".into(), "/f".into()),
            None,
        );
        // Web host over a scheme we do not claim.
        assert_eq!(
            deep_link_route("http".into(), WEB_HOST.into(), "/f".into()),
            None,
        );
        // Destinations we do not serve.
        assert_eq!(
            deep_link_route("https".into(), WEB_HOST.into(), "/pass".into()),
            None,
        );
        assert_eq!(
            deep_link_route(APP_SCHEME.into(), "pass".into(), String::new()),
            None,
        );
        // A bare app-scheme link addresses nothing.
        assert_eq!(
            deep_link_route(APP_SCHEME.into(), String::new(), String::new()),
            None,
        );
    }
}
