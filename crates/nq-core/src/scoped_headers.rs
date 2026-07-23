// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! Headers that are only attached to requests whose host matches an allowlist.
//!
//! This is a generic mechanism: callers supply the headers and the list of
//! allowed domain suffixes. It is used to attach sensitive headers (such as an
//! access token) to trusted hosts only, so the header is never sent to an
//! unrelated origin returned by a remote configuration.

use http::{HeaderMap, Uri};

/// A set of headers attached to a request only when the request's host matches
/// one of the allowed domain suffixes.
///
/// Matching uses strict label boundaries: a host matches a suffix when it is
/// equal to the suffix or ends with `.` followed by the suffix. This prevents
/// look-alike hosts such as `example.com.evil.com` from matching `example.com`.
///
/// Existing headers are never overwritten, so a header set explicitly by the
/// caller takes precedence over a scoped header with the same name.
#[derive(Debug, Clone, Default)]
pub struct ScopedHeaders {
    headers: HeaderMap,
    allowed_host_suffixes: Vec<String>,
}

impl ScopedHeaders {
    /// Create a new [`ScopedHeaders`] from the headers to attach and the list
    /// of allowed domain suffixes.
    ///
    /// Suffixes are normalized (lowercased and stripped of any trailing dot) so
    /// matching in [`allows_host`](Self::allows_host) is case-insensitive and
    /// tolerant of fully-qualified (trailing-dot) hostnames.
    pub fn new(headers: HeaderMap, allowed_host_suffixes: Vec<String>) -> Self {
        let allowed_host_suffixes = allowed_host_suffixes
            .into_iter()
            .map(|s| s.trim_end_matches('.').to_ascii_lowercase())
            .collect();

        Self {
            headers,
            allowed_host_suffixes,
        }
    }

    /// Returns whether the given host is covered by an allowed suffix, using
    /// strict label-boundary matching.
    ///
    /// Matching is case-insensitive (DNS hostnames are case-insensitive) and
    /// ignores a trailing dot on the host (fully-qualified domain names).
    pub fn allows_host(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.allowed_host_suffixes.iter().any(|suffix| {
            host == *suffix
                || (host.len() > suffix.len()
                    && host.ends_with(suffix)
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.')
        })
    }

    /// Attach the scoped headers to `headers` when the URI's host matches an
    /// allowed suffix. Headers already present in `headers` are left untouched.
    pub fn apply(&self, uri: &Uri, headers: &mut HeaderMap) {
        let Some(host) = uri.host() else {
            return;
        };

        if !self.allows_host(host) {
            return;
        }

        for (name, value) in self.headers.iter() {
            if !headers.contains_key(name) {
                headers.append(name, value.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderName, HeaderValue};

    const TEST_HEADER: &str = "x-test-token";
    const SUFFIX: &str = "speed.cloudflare.com";

    /// Build a [`ScopedHeaders`] carrying a single `x-test-token` header scoped
    /// to the given suffixes.
    fn scoped(suffixes: &[&str]) -> ScopedHeaders {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(TEST_HEADER),
            HeaderValue::from_static("secret"),
        );
        ScopedHeaders::new(
            headers,
            suffixes.iter().map(|s| s.to_string()).collect(),
        )
    }

    // --- allows_host: matching ---

    #[test]
    fn allows_exact_match() {
        assert!(scoped(&[SUFFIX]).allows_host("speed.cloudflare.com"));
    }

    #[test]
    fn allows_subdomain_match() {
        assert!(scoped(&[SUFFIX]).allows_host("staging.speed.cloudflare.com"));
    }

    #[test]
    fn allows_deep_subdomain_match() {
        assert!(scoped(&[SUFFIX]).allows_host("a.b.speed.cloudflare.com"));
    }

    #[test]
    fn allows_any_of_multiple_suffixes() {
        let scoped = scoped(&["example.com", SUFFIX]);
        assert!(scoped.allows_host("speed.cloudflare.com"));
        assert!(scoped.allows_host("www.example.com"));
    }

    // --- allows_host: case insensitivity & trailing dot ---

    #[test]
    fn allows_case_insensitive_host() {
        assert!(scoped(&[SUFFIX]).allows_host("SPEED.Cloudflare.COM"));
    }

    #[test]
    fn allows_trailing_dot_fqdn() {
        assert!(scoped(&[SUFFIX]).allows_host("speed.cloudflare.com."));
    }

    #[test]
    fn allows_trailing_dot_subdomain() {
        assert!(scoped(&[SUFFIX]).allows_host("staging.speed.cloudflare.com."));
    }

    #[test]
    fn allows_suffix_normalized_case_and_trailing_dot() {
        // Suffix supplied with mixed case and a trailing dot must still match a
        // lowercase host, verifying normalization in `new`.
        assert!(scoped(&["SPEED.Cloudflare.com."]).allows_host("speed.cloudflare.com"));
    }

    // --- allows_host: rejection (security boundary) ---

    #[test]
    fn rejects_look_alike_prefix() {
        assert!(!scoped(&[SUFFIX]).allows_host("notspeed.cloudflare.com"));
    }

    #[test]
    fn rejects_look_alike_suffix() {
        assert!(!scoped(&[SUFFIX]).allows_host("speed.cloudflare.com.evil.com"));
    }

    #[test]
    fn rejects_empty_host() {
        assert!(!scoped(&[SUFFIX]).allows_host(""));
    }

    #[test]
    fn rejects_unrelated_host() {
        assert!(!scoped(&[SUFFIX]).allows_host("example.com"));
    }

    #[test]
    fn rejects_host_shorter_than_suffix() {
        assert!(!scoped(&[SUFFIX]).allows_host("cloudflare.com"));
    }

    #[test]
    fn rejects_partial_trailing_label() {
        // Host ends with the suffix bytes but not on a label boundary.
        assert!(!scoped(&[SUFFIX]).allows_host("xspeed.cloudflare.com"));
    }

    // --- apply ---

    #[test]
    fn apply_attaches_header_to_matching_host() {
        let uri: Uri = "https://staging.speed.cloudflare.com/config"
            .parse()
            .unwrap();
        let mut headers = HeaderMap::new();
        scoped(&[SUFFIX]).apply(&uri, &mut headers);
        assert_eq!(
            headers.get(TEST_HEADER).map(|v| v.as_bytes()),
            Some(b"secret".as_slice())
        );
    }

    #[test]
    fn apply_does_not_attach_header_to_non_matching_host() {
        let uri: Uri = "https://evil.com/config".parse().unwrap();
        let mut headers = HeaderMap::new();
        scoped(&[SUFFIX]).apply(&uri, &mut headers);
        assert!(!headers.contains_key(TEST_HEADER));
    }

    #[test]
    fn apply_is_noop_when_uri_has_no_host() {
        let uri: Uri = "/relative/path".parse().unwrap();
        assert!(uri.host().is_none());
        let mut headers = HeaderMap::new();
        scoped(&[SUFFIX]).apply(&uri, &mut headers);
        assert!(headers.is_empty());
    }

    #[test]
    fn apply_does_not_overwrite_existing_header() {
        let uri: Uri = "https://speed.cloudflare.com/config".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(TEST_HEADER),
            HeaderValue::from_static("explicit"),
        );
        scoped(&[SUFFIX]).apply(&uri, &mut headers);
        assert_eq!(
            headers.get(TEST_HEADER).map(|v| v.as_bytes()),
            Some(b"explicit".as_slice())
        );
    }
}
