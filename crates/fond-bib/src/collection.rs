//! `collections/<slug>.yml` — an ordered list of citation keys with optional prose.
//! See `docs/DATA-MODEL.md`. A key here with no matching `entries/<key>.yml` is a dangling
//! reference reported by fsck, never a hard error.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{BibError, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Slug of this collection's parent, if any — makes it a child in the sidebar's tree
    /// instead of a top-level collection. `None` for a flat/top-level collection, which is
    /// how every collection worked before nesting existed, so existing files are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default)]
    pub keys: Vec<String>,
}

impl Collection {
    pub fn parse(text: &str, path: &Path) -> Result<Collection> {
        serde_yaml_ng::from_str(text).map_err(|e| BibError::PlainYaml {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    pub fn to_text(&self) -> Result<String> {
        serde_yaml_ng::to_string(self).map_err(|e| BibError::PlainYaml {
            path: Path::new("<collection>").to_path_buf(),
            message: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn round_trips() {
        let text = "name: Liberation Theology\ndescription: Sources for the chapter.\nkeys:\n- cone1970black\n- berdyaev1937destiny\n";
        let c = Collection::parse(text, &PathBuf::from("collections/lib.yml")).unwrap();
        assert_eq!(c.name, "Liberation Theology");
        assert_eq!(c.keys, vec!["cone1970black", "berdyaev1937destiny"]);
        assert_eq!(c.parent, None);
        let reparsed = Collection::parse(&c.to_text().unwrap(), &PathBuf::from("x")).unwrap();
        assert_eq!(c, reparsed);
    }

    #[test]
    fn parent_round_trips_and_is_omitted_when_absent() {
        let child = Collection {
            name: "Liberation Theology".into(),
            description: None,
            parent: Some("theology".into()),
            keys: vec!["cone1970black".into()],
        };
        let text = child.to_text().unwrap();
        assert!(text.contains("parent: theology"));
        let reparsed = Collection::parse(&text, &PathBuf::from("x")).unwrap();
        assert_eq!(reparsed.parent.as_deref(), Some("theology"));

        let top_level = Collection {
            name: "Theology".into(),
            ..Default::default()
        };
        assert!(
            !top_level.to_text().unwrap().contains("parent"),
            "a top-level collection's YAML shouldn't mention parent at all"
        );
    }
}
