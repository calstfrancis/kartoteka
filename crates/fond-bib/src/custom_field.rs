//! `custom-fields.yml` — library-wide definitions of user-added fields (§ custom fields).
//! A definition just says a field *exists* and what kind of editor/value it wants; the
//! per-entry values themselves live in each entry's own note frontmatter
//! (`NoteFrontmatter::custom_fields`), same place tags/status/rating already live, so a
//! field with no value set on a given entry costs nothing there. Defining a field here is
//! what makes it show up (empty) on every entry — "universal" per the feature's design.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{BibError, Result};

/// What kind of editor a custom field's value gets, and loosely what shape the value is.
/// All three are stored as plain strings in `NoteFrontmatter::custom_fields` regardless —
/// this only selects the widget and (for `Number`) whether the stored string is validated
/// as numeric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CustomFieldType {
    /// Free text, single line.
    Text,
    /// Free text, but validated/rendered as a number (an entry, not a stepper — most
    /// custom numeric fields, e.g. a page count or a rating on a different scale, aren't
    /// naturally "step by one from what's already there").
    Number,
    /// Comma-separated values, chip-style — the same interaction as the built-in Tags
    /// field, just under a name of the user's choosing (e.g. "Methodology").
    Tag,
}

/// One user-defined field, applying to every entry in the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomFieldDef {
    /// Display name, and the key `NoteFrontmatter::custom_fields` stores its value under.
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: CustomFieldType,
}

/// The library-wide list of custom field definitions, in display order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomFieldDefs {
    #[serde(default)]
    pub fields: Vec<CustomFieldDef>,
}

impl CustomFieldDefs {
    pub fn parse(text: &str, path: &Path) -> Result<CustomFieldDefs> {
        if text.trim().is_empty() {
            return Ok(CustomFieldDefs::default());
        }
        serde_yaml_ng::from_str(text).map_err(|e| BibError::PlainYaml {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    pub fn to_text(&self) -> Result<String> {
        serde_yaml_ng::to_string(self).map_err(|e| BibError::PlainYaml {
            path: Path::new("<custom-fields>").to_path_buf(),
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
        let defs = CustomFieldDefs {
            fields: vec![
                CustomFieldDef {
                    name: "Methodology".to_string(),
                    field_type: CustomFieldType::Tag,
                },
                CustomFieldDef {
                    name: "Page count".to_string(),
                    field_type: CustomFieldType::Number,
                },
            ],
        };
        let text = defs.to_text().unwrap();
        let reparsed = CustomFieldDefs::parse(&text, &PathBuf::from("custom-fields.yml")).unwrap();
        assert_eq!(defs, reparsed);
    }

    #[test]
    fn empty_file_parses_as_no_fields() {
        let defs = CustomFieldDefs::parse("", &PathBuf::from("custom-fields.yml")).unwrap();
        assert!(defs.fields.is_empty());
    }
}
