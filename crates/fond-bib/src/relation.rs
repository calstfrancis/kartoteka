//! Typed relationships between library items. See `docs/M2-SPEC.md` §1.
//!
//! A note's frontmatter carries a list of [`Relation`] edges. Each edge is either
//! *forward* (authored by the user on this item) or *inverse* (`inverse: true`,
//! maintained by Kartoteka from some other item's forward edge). The governing invariant:
//! **the set of forward edges is the whole truth; every inverse edge is a function of it**,
//! so [`Library::fsck`](crate::Library::fsck) can regenerate all inverse edges from the
//! forward ones with no information loss.

use serde::{Deserialize, Serialize};

/// The closed relationship vocabulary. Each predicate has exactly one inverse; the
/// symmetric [`Predicate::Related`] is its own inverse. Unknown predicates are a hard parse
/// error (via serde) so a typo can never become a silent broken edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Predicate {
    Cites,
    CitedBy,
    Critiques,
    CritiquedBy,
    Reviews,
    ReviewedBy,
    CommentaryOn,
    HasCommentary,
    TranslationOf,
    HasTranslation,
    EditionOf,
    HasEdition,
    Supersedes,
    SupersededBy,
    RepliesTo,
    RepliedToBy,
    Expands,
    ExpandedBy,
    /// Untyped "see also"; symmetric (its own inverse).
    Related,
}

impl Predicate {
    /// The inverse predicate: the one that holds on the *other* endpoint of the edge.
    /// `A cites B` implies `B cited-by A`, so `Cites.inverse() == CitedBy`.
    pub fn inverse(self) -> Predicate {
        use Predicate::*;
        match self {
            Cites => CitedBy,
            CitedBy => Cites,
            Critiques => CritiquedBy,
            CritiquedBy => Critiques,
            Reviews => ReviewedBy,
            ReviewedBy => Reviews,
            CommentaryOn => HasCommentary,
            HasCommentary => CommentaryOn,
            TranslationOf => HasTranslation,
            HasTranslation => TranslationOf,
            EditionOf => HasEdition,
            HasEdition => EditionOf,
            Supersedes => SupersededBy,
            SupersededBy => Supersedes,
            RepliesTo => RepliedToBy,
            RepliedToBy => RepliesTo,
            Expands => ExpandedBy,
            ExpandedBy => Expands,
            Related => Related,
        }
    }

    /// True when the predicate is its own inverse (a symmetric relation, i.e. `Related`).
    pub fn is_self_inverse(self) -> bool {
        self.inverse() == self
    }
}

/// One relationship edge as stored in a note's frontmatter (`relations:` list).
///
/// Identity for set operations is the pair `(predicate, target)`; the `inverse` flag is
/// metadata about who maintains the edge, not part of its identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Relation {
    pub predicate: Predicate,
    /// Target citation key (M2) or node slug (M3 — the resolver accepts either).
    pub target: String,
    /// True for a Kartoteka-maintained inverse edge; false/absent for a user-authored one.
    /// Inverse edges are derived — `fsck --fix` rebuilds them from forward edges.
    #[serde(default, skip_serializing_if = "is_false")]
    pub inverse: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Relation {
    /// A user-authored forward edge (`inverse: false`).
    pub fn forward(predicate: Predicate, target: impl Into<String>) -> Relation {
        Relation {
            predicate,
            target: target.into(),
            inverse: false,
        }
    }

    /// The `(predicate, target)` pair that identifies this edge, ignoring `inverse`.
    pub fn identity(&self) -> (Predicate, &str) {
        (self.predicate, self.target.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_predicate_inverts_symmetrically() {
        // inverse of the inverse is the original, for all variants.
        for p in [
            Predicate::Cites,
            Predicate::CitedBy,
            Predicate::Critiques,
            Predicate::CritiquedBy,
            Predicate::Reviews,
            Predicate::ReviewedBy,
            Predicate::CommentaryOn,
            Predicate::HasCommentary,
            Predicate::TranslationOf,
            Predicate::HasTranslation,
            Predicate::EditionOf,
            Predicate::HasEdition,
            Predicate::Supersedes,
            Predicate::SupersededBy,
            Predicate::RepliesTo,
            Predicate::RepliedToBy,
            Predicate::Expands,
            Predicate::ExpandedBy,
            Predicate::Related,
        ] {
            assert_eq!(p.inverse().inverse(), p, "double-inverse must be identity");
        }
    }

    #[test]
    fn related_is_the_only_self_inverse() {
        assert!(Predicate::Related.is_self_inverse());
        assert!(!Predicate::Cites.is_self_inverse());
        assert!(!Predicate::CitedBy.is_self_inverse());
    }

    #[test]
    fn serializes_kebab_case() {
        let r = Relation::forward(Predicate::TranslationOf, "gutierrez1971teologia");
        let yaml = serde_yaml_ng::to_string(&r).unwrap();
        assert!(yaml.contains("predicate: translation-of"), "{yaml}");
        assert!(yaml.contains("target: gutierrez1971teologia"), "{yaml}");
        // A forward edge omits `inverse` entirely.
        assert!(!yaml.contains("inverse"), "{yaml}");
    }

    #[test]
    fn inverse_edge_serializes_the_flag() {
        let r = Relation {
            predicate: Predicate::CitedBy,
            target: "cone1975god".into(),
            inverse: true,
        };
        let yaml = serde_yaml_ng::to_string(&r).unwrap();
        assert!(yaml.contains("inverse: true"), "{yaml}");
    }

    #[test]
    fn unknown_predicate_is_a_parse_error() {
        let err = serde_yaml_ng::from_str::<Relation>("predicate: frobnicates\ntarget: x\n");
        assert!(err.is_err(), "unknown predicate must fail to parse");
    }
}
