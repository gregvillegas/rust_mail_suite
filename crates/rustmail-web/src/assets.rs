//! Static asset URLs, cache busting and the CDN/local choice.
//!
//! Every file under the static directory is hashed at startup. Templates link
//! to `/static/<path>?v=<hash>`, so a URL changes exactly when its content does
//! and can be cached for a year. Requests without `v=` are revalidated instead.
//!
//! Third-party libraries (Bootstrap, htmx, Alpine) are vendored under
//! `static/vendor/` and, in [`AssetSource::Cdn`] mode, are *also* offered from
//! jsDelivr with Subresource Integrity. `asset-fallback.js` loads the local copy
//! if the CDN copy did not arrive or failed its integrity check, so the site
//! works offline and on air-gapped networks. The vendored files are
//! byte-identical to the CDN ones (verified against a second CDN when added).

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// URL prefix the static directory is served under.
pub const STATIC_PREFIX: &str = "/static";

/// Where third-party libraries are loaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetSource {
    /// jsDelivr with integrity hashes, falling back to the vendored copy.
    Cdn,
    /// Always the vendored copy. Best for air-gapped or privacy-sensitive installs.
    Local,
}

impl AssetSource {
    /// Parses `cdn` or `local` (case-insensitive).
    ///
    /// # Errors
    /// Returns a message naming the accepted values.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cdn" => Ok(Self::Cdn),
            "local" => Ok(Self::Local),
            other => Err(format!(
                "ASSET_SOURCE must be 'cdn' or 'local', got '{other}'"
            )),
        }
    }
}

/// A vendored third-party file and where its CDN twin lives.
#[derive(Debug, Clone, Copy)]
pub struct Vendor {
    /// Path under the static directory.
    pub local: &'static str,
    /// Absolute CDN URL.
    pub cdn: &'static str,
    /// Subresource Integrity value (`sha384-...`).
    pub integrity: &'static str,
}

/// Bootstrap 5.3.9 stylesheet.
pub const BOOTSTRAP_CSS: Vendor = Vendor {
    local: "vendor/bootstrap/5.3.9/bootstrap.min.css",
    cdn: "https://cdn.jsdelivr.net/npm/bootstrap@5.3.9/dist/css/bootstrap.min.css",
    integrity: "sha384-yEPZ7jP9S0QKiUQ0m3jjsO4ProIW7wvR9gXkI7JOGa2XVsw53Yke2bsdkBbjmmiX",
};

/// htmx 2.0.11.
pub const HTMX: Vendor = Vendor {
    local: "vendor/htmx/2.0.11/htmx.min.js",
    cdn: "https://cdn.jsdelivr.net/npm/htmx.org@2.0.11/dist/htmx.min.js",
    integrity: "sha384-2OatzQy1H+Zd/IIrjr1TcuDGqLXeHhbooAyJY1KdQMKnr4LZ22k31GBLdYKHmVjg",
};

/// Alpine.js 3.17.5, CSP build (no `eval`, so no `unsafe-eval` in the CSP).
pub const ALPINE: Vendor = Vendor {
    local: "vendor/alpine-csp/3.17.5/alpine-csp.min.js",
    cdn: "https://cdn.jsdelivr.net/npm/@alpinejs/csp@3.17.5/dist/cdn.min.js",
    integrity: "sha384-Q4UOujiwqAb/jTt3ki4pdkETk4I3rIFQX8h271PLI41okj/YIKY7IQQFMw73wkrX",
};

/// How a template should reference one library.
#[derive(Debug, Clone)]
pub struct AssetRef {
    /// The URL to load first.
    pub href: String,
    /// Integrity hash, present only for CDN URLs.
    pub integrity: Option<&'static str>,
    /// Local URL to use if `href` fails, present only in CDN mode.
    pub fallback: Option<String>,
}

/// Hashed static files plus the active [`AssetSource`].
pub struct Assets {
    source: AssetSource,
    hashes: HashMap<String, String>,
}

impl fmt::Debug for Assets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Assets")
            .field("source", &self.source)
            .field("files", &self.hashes.len())
            .finish()
    }
}

impl Assets {
    /// Hashes every file under `static_dir`.
    ///
    /// # Errors
    /// Fails if the directory cannot be read, or a vendored file that templates
    /// depend on is missing (better to refuse to start than to serve a broken UI).
    pub fn load(static_dir: &Path, source: AssetSource) -> io::Result<Self> {
        let mut hashes = HashMap::new();
        walk(static_dir, static_dir, &mut hashes)?;
        for required in [
            BOOTSTRAP_CSS.local,
            HTMX.local,
            ALPINE.local,
            "css/app.css",
            "js/app.js",
        ] {
            if !hashes.contains_key(required) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "static asset '{required}' not found under {}",
                        static_dir.display()
                    ),
                ));
            }
        }
        Ok(Self { source, hashes })
    }

    /// The active source for third-party libraries.
    #[must_use]
    pub fn source(&self) -> AssetSource {
        self.source
    }

    /// `/static/<path>?v=<hash>`; without the hash if the file is unknown.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        match self.hashes.get(path) {
            Some(hash) => format!("{STATIC_PREFIX}/{path}?v={hash}"),
            None => {
                tracing::warn!(path, "asset not found in manifest");
                format!("{STATIC_PREFIX}/{path}")
            }
        }
    }

    /// Reference for a vendored library according to the active source.
    #[must_use]
    pub fn vendor(&self, vendor: &Vendor) -> AssetRef {
        match self.source {
            AssetSource::Local => AssetRef {
                href: self.url(vendor.local),
                integrity: None,
                fallback: None,
            },
            AssetSource::Cdn => AssetRef {
                href: vendor.cdn.to_owned(),
                integrity: Some(vendor.integrity),
                fallback: Some(self.url(vendor.local)),
            },
        }
    }

    /// Whether third-party libraries come from the CDN (so the local-fallback
    /// script is worth loading).
    #[must_use]
    pub fn uses_cdn(&self) -> bool {
        self.source == AssetSource::Cdn
    }

    /// Bootstrap stylesheet reference.
    #[must_use]
    pub fn bootstrap_css(&self) -> AssetRef {
        self.vendor(&BOOTSTRAP_CSS)
    }

    /// htmx script reference.
    #[must_use]
    pub fn htmx(&self) -> AssetRef {
        self.vendor(&HTMX)
    }

    /// Alpine.js script reference.
    #[must_use]
    pub fn alpine(&self) -> AssetRef {
        self.vendor(&ALPINE)
    }

    /// The `Content-Security-Policy` matching the active source.
    ///
    /// No inline scripts or styles, no `eval`; the CDN origin is allowed only
    /// in CDN mode.
    #[must_use]
    pub fn content_security_policy(&self) -> String {
        let cdn = match self.source {
            AssetSource::Cdn => " https://cdn.jsdelivr.net",
            AssetSource::Local => "",
        };
        format!(
            "default-src 'self'; script-src 'self'{cdn}; style-src 'self'{cdn}; \
             img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; \
             base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
        )
    }
}

fn walk(root: &Path, dir: &Path, out: &mut HashMap<String, String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk(root, &path, out)?;
        } else if let Some(relative) = relative_url_path(root, &path) {
            out.insert(relative, short_hash(&fs::read(&path)?));
        }
    }
    Ok(())
}

/// `dir/file.ext` relative to `root` with forward slashes, or `None` for
/// names that are not valid UTF-8 (they could not appear in a template anyway).
fn relative_url_path(root: &Path, path: &Path) -> Option<String> {
    let relative: PathBuf = path.strip_prefix(root).ok()?.to_path_buf();
    let parts: Option<Vec<&str>> = relative
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect();
    Some(parts?.join("/"))
}

fn short_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .take(5)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn static_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static")
    }

    fn assets(source: AssetSource) -> Assets {
        Assets::load(&static_dir(), source).expect("assets load")
    }

    #[test]
    fn urls_carry_a_content_hash() {
        let a = assets(AssetSource::Local);
        let url = a.url("css/app.css");
        let (path, query) = url.split_once("?v=").expect("versioned");
        assert_eq!(path, "/static/css/app.css");
        assert_eq!(query.len(), 10);
        assert!(query.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(url, a.url("css/app.css"), "stable across calls");
        assert_ne!(a.url("css/app.css"), a.url("js/app.js"));
    }

    #[test]
    fn unknown_files_get_an_unversioned_url_instead_of_failing() {
        assert_eq!(
            assets(AssetSource::Local).url("nope.css"),
            "/static/nope.css"
        );
    }

    #[test]
    fn local_mode_uses_vendored_files_without_integrity() {
        let r = assets(AssetSource::Local).bootstrap_css();
        assert!(r.href.starts_with("/static/vendor/bootstrap/5.3.9/"));
        assert!(r.integrity.is_none() && r.fallback.is_none());
    }

    #[test]
    fn cdn_mode_adds_integrity_and_a_local_fallback() {
        let a = assets(AssetSource::Cdn);
        for (r, name) in [
            (a.bootstrap_css(), "bootstrap"),
            (a.htmx(), "htmx"),
            (a.alpine(), "alpine"),
        ] {
            assert!(r.href.starts_with("https://cdn.jsdelivr.net/"), "{name}");
            assert!(
                r.integrity.is_some_and(|i| i.starts_with("sha384-")),
                "{name}"
            );
            assert!(
                r.fallback
                    .as_deref()
                    .is_some_and(|f| f.starts_with("/static/vendor/")),
                "{name}"
            );
        }
    }

    /// The SRI constants in this file must match the vendored bytes, or the
    /// CDN and local copies have drifted apart.
    #[test]
    fn integrity_constants_match_the_vendored_files() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        use sha2::Sha384;
        for vendor in [BOOTSTRAP_CSS, HTMX, ALPINE] {
            let bytes = fs::read(static_dir().join(vendor.local)).expect("vendored file");
            let expected = format!("sha384-{}", STANDARD.encode(Sha384::digest(&bytes)));
            assert_eq!(vendor.integrity, expected, "{}", vendor.local);
        }
    }

    #[test]
    fn csp_is_strict_and_only_mentions_the_cdn_when_used() {
        let cdn = assets(AssetSource::Cdn).content_security_policy();
        let local = assets(AssetSource::Local).content_security_policy();
        assert!(cdn.contains("script-src 'self' https://cdn.jsdelivr.net;"));
        assert!(!local.contains("jsdelivr"));
        for csp in [&cdn, &local] {
            assert!(!csp.contains("unsafe-inline") && !csp.contains("unsafe-eval"));
            assert!(csp.contains("object-src 'none'") && csp.contains("frame-ancestors 'none'"));
        }
    }

    #[test]
    fn source_parsing() {
        assert_eq!(AssetSource::parse(" CDN ").expect("cdn"), AssetSource::Cdn);
        assert_eq!(
            AssetSource::parse("local").expect("local"),
            AssetSource::Local
        );
        assert!(AssetSource::parse("both").is_err());
    }

    #[test]
    fn a_missing_required_file_stops_startup() {
        let dir = std::env::temp_dir().join(format!("rm-assets-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let err = Assets::load(&dir, AssetSource::Local).expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        fs::remove_dir_all(&dir).ok();
    }
}
