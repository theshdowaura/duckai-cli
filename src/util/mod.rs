use std::collections::BTreeSet;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use once_cell::sync::Lazy;
use regex::Regex;
use sha2::{Digest, Sha256};

static CHROME_VERSION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"Chrome/(\d{2,3})").expect("regex should compile"));

/// Extracts the Chrome major version from a UA string (defaulting to `"140"`).
pub fn chrome_major_version(ua: &str) -> String {
    CHROME_VERSION_RE
        .captures(ua)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_owned())
        .unwrap_or_else(|| "140".to_owned())
}

/// Best-effort platform detection for Sec-CH-UA-Platform.
pub fn platform_token(ua: &str) -> &'static str {
    if ua.contains("Mac OS X") {
        "macOS"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("X11; Linux") {
        "Linux"
    } else {
        "Windows"
    }
}

/// Builds a Sec-CH-UA header string mirroring Chromium style.
pub fn sec_ch_ua(ua: &str) -> String {
    let major = chrome_major_version(ua);
    format!(r#""Chromium";v="{major}", "Not=A?Brand";v="24", "Google Chrome";v="{major}""#)
}

/// Computes a SHA-256 digest encoded as standard Base64.
pub fn sha256_base64(value: impl AsRef<[u8]>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_ref());
    let digest = hasher.finalize();
    BASE64_STANDARD.encode(digest)
}

/// Parses user-provided selections into a deduplicated set of indices.
pub fn parse_tile_selection(input: &str, len: usize) -> Vec<usize> {
    let mut indices = BTreeSet::new();
    for token in input.split(|c: char| c.is_ascii_whitespace() || c == ',') {
        if token.is_empty() {
            continue;
        }
        if let Ok(value) = token.parse::<usize>() {
            if value < len {
                indices.insert(value);
            }
        }
    }
    indices.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_chrome_version() {
        let ua = "Mozilla/5.0 ... Chrome/141.0.1234.89 Safari/537.36";
        assert_eq!(chrome_major_version(ua), "141");
    }

    #[test]
    fn defaults_chrome_version() {
        let ua = "UnknownAgent/1.0";
        assert_eq!(chrome_major_version(ua), "140");
    }

    #[test]
    fn platform_detection_variants() {
        assert_eq!(platform_token("...Mac OS X..."), "macOS");
        assert_eq!(platform_token("...Android..."), "Android");
        assert_eq!(platform_token("X11; Linux x86_64"), "Linux");
        assert_eq!(platform_token("Windows"), "Windows");
    }

    #[test]
    fn sec_ch_header_format() {
        let ua = "Mozilla/5.0 ... Chrome/141.0.1234.89 Safari/537.36";
        let header = sec_ch_ua(ua);
        assert!(header.contains(r#""Chromium";v="141""#));
        assert!(header.contains(r#""Google Chrome";v="141""#));
    }

    #[test]
    fn hashes_base64() {
        let digest = sha256_base64("hello");
        assert_eq!(digest, "LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=");
    }

    #[test]
    fn parses_tile_indices() {
        let input = "0, 3 4, 4, 2";
        assert_eq!(parse_tile_selection(input, 5), vec![0, 2, 3, 4]);
    }

    #[test]
    fn ignores_out_of_bounds() {
        let input = "1, 9, -1, 2";
        assert_eq!(parse_tile_selection(input, 3), vec![1, 2]);
    }
}
