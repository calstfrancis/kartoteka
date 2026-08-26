//! `ai/<key>.yml` — AI-generated metadata sidecar. See `docs/M2-SPEC.md` §4 and
//! `docs/DATA-MODEL-EXTENSIONS.md` §5.
//!
//! Committed (versioned) but **never authoritative over curated data**: it is regenerable
//! from `source_hash`, carries a provenance header identifying the model, and is safe to
//! `rm` en masse. The hard boundary is enforced structurally — nothing in the write path
//! ever copies these fields into `NoteFrontmatter` or `entries/`. Promoting an AI keyword
//! into a real tag is an explicit user action, one-directional, never automatic.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{BibError, Result};

/// The current on-disk schema version for an AI sidecar.
pub const AI_SCHEMA: u32 = 1;

/// AI-generated metadata for one library key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AiMetadata {
    /// On-disk schema version (`= AI_SCHEMA`), guarding future field changes.
    pub schema: u32,
    /// Model id that produced this, e.g. `claude-opus-4-8` — provenance so a bad batch is
    /// identifiable and safe to purge.
    pub generated_by: String,
    /// RFC3339 timestamp of generation.
    pub generated_at: String,
    /// Content hash of the attachment/text this was generated from, so a stale sidecar is
    /// detectable after the source changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub concepts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counterarguments: Vec<String>,
}

impl AiMetadata {
    /// A fresh sidecar with the current schema and the given provenance.
    pub fn new(generated_by: impl Into<String>, generated_at: impl Into<String>) -> AiMetadata {
        AiMetadata {
            schema: AI_SCHEMA,
            generated_by: generated_by.into(),
            generated_at: generated_at.into(),
            source_hash: None,
            summary: None,
            keywords: Vec::new(),
            concepts: Vec::new(),
            claims: Vec::new(),
            counterarguments: Vec::new(),
        }
    }

    pub fn parse(text: &str, path: &Path) -> Result<AiMetadata> {
        serde_yaml_ng::from_str(text).map_err(|e| BibError::Yaml {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    pub fn to_yaml(&self) -> Result<String> {
        serde_yaml_ng::to_string(self).map_err(|e| BibError::Yaml {
            path: Path::new("<ai>").to_path_buf(),
            message: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn round_trips_through_yaml() {
        let mut ai = AiMetadata::new("claude-opus-4-8", "2026-07-24T10:00:00Z");
        ai.source_hash = Some("blake3:9f2bc4".into());
        ai.summary = Some("Argues the gospel is intrinsically liberating.".into());
        ai.keywords = vec!["liberation".into(), "christology".into()];
        ai.claims = vec!["The gospel is a gospel of liberation.".into()];

        let yaml = ai.to_yaml().unwrap();
        let back = AiMetadata::parse(&yaml, &PathBuf::from("ai/x.yml")).unwrap();
        assert_eq!(ai, back);
        assert_eq!(back.schema, AI_SCHEMA);
    }

    #[test]
    fn empty_collections_are_elided() {
        let ai = AiMetadata::new("m", "t");
        let yaml = ai.to_yaml().unwrap();
        assert!(
            !yaml.contains("keywords"),
            "empty keywords should be elided: {yaml}"
        );
        assert!(
            !yaml.contains("source-hash"),
            "absent source-hash elided: {yaml}"
        );
    }
}
