//! `nodes/<slug>.md` — first-class non-work entities (people, concepts, schools, events,
//! places) and the relationships among them and to citation entries. See `docs/M3-SPEC.md`.
//!
//! A node is structurally a note (YAML frontmatter + Markdown prose), so the knowledge
//! graph is homogeneous: a relation `target` may be a citation key **or** a node slug,
//! resolved against `entries/` first, then `nodes/` (the resolver lands in a later M3 PR).
//! This is the home for author-level identifiers (ORCID / VIAF / Wikidata) that cannot live
//! in a pure-Hayagriva entry file.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{BibError, Result};
use crate::relation::Relation;

/// The kind of non-work entity a node represents. `WorkUncataloged` is for a referenced
/// work that has no `entries/` record yet (e.g. cited but not acquired).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeType {
    Person,
    Concept,
    School,
    Event,
    Place,
    WorkUncataloged,
}

/// A node file with an omitted `node-type` defaults to `Concept` — the most forgiving
/// choice for a hand-authored file, and what lets [`NodeFrontmatter`] derive `Default`.
/// (Resolves `docs/M3-SPEC.md` open item 1 in favour of a default over a hard requirement.)
impl Default for NodeType {
    fn default() -> Self {
        NodeType::Concept
    }
}

/// The YAML frontmatter of a `nodes/<slug>.md` file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NodeFrontmatter {
    /// The entity kind; defaults to [`NodeType::Concept`] when omitted.
    #[serde(default)]
    pub node_type: NodeType,
    /// Human-readable display name. The slug is derived from this but stays stable once
    /// assigned even if the label is later edited.
    pub label: String,
    /// Alternate names/spellings, for search and disambiguation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Free-form external identifiers (wikidata, viaf, orcid, lccn, …). A `BTreeMap` keeps
    /// key order stable for clean diffs; unknown keys are allowed (extensibility).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub identifiers: BTreeMap<String, String>,
    /// Same [`Relation`] type as notes — forward edges plus Kartoteka-maintained inverse
    /// edges. A `target` may be an entry key or another node slug.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
}

/// A parsed node: frontmatter plus the Markdown body prose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    pub frontmatter: NodeFrontmatter,
    pub body: String,
}

impl Node {
    /// Parse a `nodes/<slug>.md` file body. Unlike a note, a node's frontmatter is
    /// mandatory (`label` at minimum), so a file with no `---` block is a parse error rather
    /// than an all-defaults node. `path` is used only for errors.
    pub fn parse(text: &str, path: &Path) -> Result<Node> {
        let Some((yaml, body)) = crate::util::split_frontmatter(text) else {
            return Err(BibError::Frontmatter {
                path: path.to_path_buf(),
                message: "node file has no `---` frontmatter block (a node needs at least a \
                          `label`)"
                    .into(),
            });
        };

        let frontmatter: NodeFrontmatter =
            serde_yaml_ng::from_str(yaml).map_err(|e| BibError::Frontmatter {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;

        if frontmatter.label.trim().is_empty() {
            return Err(BibError::Frontmatter {
                path: path.to_path_buf(),
                message: "node frontmatter is missing a non-empty `label`".into(),
            });
        }

        Ok(Node {
            frontmatter,
            body: body.to_string(),
        })
    }

    /// Serialize back to file text: `---`-fenced frontmatter followed by the body. Round-
    /// trips with [`Node::parse`].
    pub fn to_text(&self) -> Result<String> {
        let yaml =
            serde_yaml_ng::to_string(&self.frontmatter).map_err(|e| BibError::Frontmatter {
                path: Path::new("<node>").to_path_buf(),
                message: e.to_string(),
            })?;
        Ok(format!("---\n{yaml}---\n{}", self.body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relation::Predicate;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("nodes/test.md")
    }

    #[test]
    fn parses_full_frontmatter() {
        // Uses only the M2 predicate vocabulary; the node-oriented predicates
        // (`influenced`, `authored`, …) arrive in the next M3 PR.
        let text = "---\nnode-type: person\nlabel: Augustine of Hippo\naliases: [Augustine, Aurelius Augustinus]\nidentifiers:\n  wikidata: Q8018\n  viaf: \"66806872\"\nrelations:\n  - predicate: related\n    target: aquinas\n  - predicate: cites\n    target: augustine0426city\n---\nBishop of Hippo, 354\u{2013}430.\n";
        let node = Node::parse(text, &p()).unwrap();
        assert_eq!(node.frontmatter.node_type, NodeType::Person);
        assert_eq!(node.frontmatter.label, "Augustine of Hippo");
        assert_eq!(
            node.frontmatter.aliases,
            vec!["Augustine", "Aurelius Augustinus"]
        );
        assert_eq!(
            node.frontmatter
                .identifiers
                .get("wikidata")
                .map(String::as_str),
            Some("Q8018")
        );
        assert_eq!(
            node.frontmatter.identifiers.get("viaf").map(String::as_str),
            Some("66806872")
        );
        assert_eq!(node.frontmatter.relations.len(), 2);
        assert_eq!(node.frontmatter.relations[0].predicate, Predicate::Related);
        assert_eq!(node.frontmatter.relations[0].target, "aquinas");
        assert_eq!(node.body, "Bishop of Hippo, 354\u{2013}430.\n");
    }

    #[test]
    fn node_type_defaults_to_concept() {
        let text = "---\nlabel: Divine Simplicity\n---\nA doctrine about God's non-composition.\n";
        let node = Node::parse(text, &p()).unwrap();
        assert_eq!(node.frontmatter.node_type, NodeType::Concept);
        assert!(node.frontmatter.aliases.is_empty());
        assert!(node.frontmatter.identifiers.is_empty());
    }

    #[test]
    fn missing_frontmatter_is_a_parse_error() {
        let err = Node::parse("Just prose, no frontmatter.\n", &p());
        assert!(matches!(err, Err(BibError::Frontmatter { .. })));
    }

    #[test]
    fn missing_or_empty_label_is_a_parse_error() {
        // Absent `label`.
        let err = Node::parse("---\nnode-type: place\n---\nBody.\n", &p());
        assert!(err.is_err(), "a node without a label must fail to parse");
        // Present but blank.
        let err = Node::parse("---\nlabel: \"   \"\n---\nBody.\n", &p());
        assert!(matches!(err, Err(BibError::Frontmatter { .. })));
    }

    #[test]
    fn unknown_node_type_is_a_parse_error() {
        let err = Node::parse("---\nnode-type: dragon\nlabel: Smaug\n---\nBody.\n", &p());
        assert!(err.is_err(), "an unknown node-type must fail to parse");
    }

    #[test]
    fn round_trips_through_model() {
        let text = "---\nnode-type: school\nlabel: Cappadocian Fathers\naliases: [Cappadocians]\nidentifiers:\n  wikidata: Q1035960\nrelations:\n  - predicate: related\n    target: basil\n---\nFourth-century theologians.\n";
        let node = Node::parse(text, &p()).unwrap();
        let reparsed = Node::parse(&node.to_text().unwrap(), &p()).unwrap();
        assert_eq!(node, reparsed);
    }

    #[test]
    fn to_text_is_kebab_case() {
        let node = Node {
            frontmatter: NodeFrontmatter {
                node_type: NodeType::WorkUncataloged,
                label: "Enneads".into(),
                ..Default::default()
            },
            body: "Referenced but not yet catalogued.\n".into(),
        };
        let text = node.to_text().unwrap();
        assert!(text.contains("node-type: work-uncataloged"), "{text}");
        assert!(text.contains("label: Enneads"), "{text}");
    }
}
