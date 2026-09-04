//! Translating between the store's internal subjects and the public URLs the
//! same data is served under.
//!
//! Atomic Data addresses locally-hosted resources as `internal:/path`, which a
//! server rewrites to `<base>/path` when it serves them. That rewrite is
//! exactly what the minted ontology depends on: a class or property is only
//! useful to a consumer if its subject resolves, so the terms are stored as
//! `internal:/path/to/property` and published as
//! `https://my-ontologies.com/path/to/property`.
//!
//! Keeping the pair in one place means the store never has to hold absolute
//! URLs — moving the deployment to a different origin is a config change, not
//! a data migration.

/// The `internal:` scheme prefix, including the root slash Atomic Data's
/// `Subject::Internal` form carries (`internal:/path`).
pub const INTERNAL_PREFIX: &str = "internal:/";

/// Maps between `internal:/…` subjects and their public `https://…` URLs.
#[derive(Clone, Debug)]
pub struct SubjectMapper {
    /// Public origin (and optional base path), without a trailing slash.
    public_url: String,
}

impl SubjectMapper {
    /// `public_url` must already be normalized (absolute, no trailing slash);
    /// [`crate::config::Config::from_env`] guarantees that.
    pub fn new(public_url: impl Into<String>) -> Self {
        SubjectMapper {
            public_url: public_url.into(),
        }
    }

    pub fn public_url(&self) -> &str {
        &self.public_url
    }

    /// `path/to/property` → `internal:/path/to/property`.
    pub fn internal(&self, path: &str) -> String {
        format!("{INTERNAL_PREFIX}{}", path.trim_start_matches('/'))
    }

    /// `path/to/property` → `https://my-ontologies.com/path/to/property`.
    pub fn public(&self, path: &str) -> String {
        format!("{}/{}", self.public_url, path.trim_start_matches('/'))
    }

    /// The public URL a stored subject is served as. Returns `None` for a
    /// subject that is not internal — an external `https://atomicdata.dev/…`
    /// property is already canonical and must be left alone.
    pub fn to_public(&self, subject: &str) -> Option<String> {
        subject
            .strip_prefix(INTERNAL_PREFIX)
            .map(|path| self.public(path))
    }

    /// The internal subject a public URL of ours is stored under. Returns
    /// `None` for a URL under some other origin.
    pub fn to_internal(&self, url: &str) -> Option<String> {
        let rest = url.strip_prefix(&self.public_url)?;
        // Guard against `https://my-ontologies.com.evil.example/x` matching the
        // prefix: what follows the origin must be a path separator (or nothing).
        match rest.chars().next() {
            None => Some(INTERNAL_PREFIX.to_owned()),
            Some('/') => Some(self.internal(rest)),
            Some(_) => None,
        }
    }

    /// Resolves an ontology cross-reference, which the engine may give either
    /// as a path of its own ontology or as an absolute URL (e.g. a builtin
    /// `https://atomicdata.dev/properties/description`). Ours become internal
    /// subjects; anybody else's are kept verbatim.
    pub fn resolve_reference(&self, reference: &str) -> String {
        if reference.starts_with("http://") || reference.starts_with("https://") {
            self.to_internal(reference)
                .unwrap_or_else(|| reference.to_owned())
        } else if reference.starts_with(INTERNAL_PREFIX) {
            reference.to_owned()
        } else {
            self.internal(reference)
        }
    }
}

/// Escapes a path segment so a value containing `/` cannot forge extra path
/// structure — a repository namespace is `owner/repo`, and it has to survive
/// a round trip as a single segment.
pub fn encode_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Inverse of [`encode_segment`]. Invalid escapes are returned unchanged
/// rather than dropped, so a hand-written subject never silently loses text.
pub fn decode_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                decoded.push(byte);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper() -> SubjectMapper {
        SubjectMapper::new("https://my-ontologies.com")
    }

    #[test]
    fn internal_subject_matches_its_public_url() {
        let mapper = mapper();
        assert_eq!(
            mapper.internal("path/to/property"),
            "internal:/path/to/property"
        );
        assert_eq!(
            mapper.public("path/to/property"),
            "https://my-ontologies.com/path/to/property"
        );
        assert_eq!(
            mapper
                .to_public(&mapper.internal("path/to/property"))
                .unwrap(),
            mapper.public("path/to/property")
        );
    }

    #[test]
    fn public_url_round_trips_back_to_internal() {
        let mapper = mapper();
        let public = mapper.public("github-issues/property/title");
        assert_eq!(
            mapper.to_internal(&public).unwrap(),
            "internal:/github-issues/property/title"
        );
    }

    #[test]
    fn foreign_subjects_are_left_alone() {
        let mapper = mapper();
        assert_eq!(
            mapper.to_public("https://atomicdata.dev/properties/name"),
            None
        );
        assert_eq!(mapper.to_internal("https://example.com/x"), None);
        // A lookalike origin must not be mistaken for ours.
        assert_eq!(
            mapper.to_internal("https://my-ontologies.com.evil.example/x"),
            None
        );
    }

    #[test]
    fn references_resolve_by_origin() {
        let mapper = mapper();
        assert_eq!(
            mapper.resolve_reference("github-issues/property/title"),
            "internal:/github-issues/property/title"
        );
        assert_eq!(
            mapper.resolve_reference("https://my-ontologies.com/github-issues/property/title"),
            "internal:/github-issues/property/title"
        );
        assert_eq!(
            mapper.resolve_reference("https://atomicdata.dev/properties/description"),
            "https://atomicdata.dev/properties/description"
        );
    }

    #[test]
    fn segments_round_trip_through_encoding() {
        for segment in ["localthought/test-repo-1", "issue", "42", "a b/c%d"] {
            assert_eq!(decode_segment(&encode_segment(segment)), segment);
        }
        assert!(!encode_segment("localthought/test-repo-1").contains('/'));
    }
}
