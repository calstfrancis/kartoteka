//! Typed relationships between library items. See `docs/M2-SPEC.md` §1.
//!
//! A note's frontmatter carries a list of [`Relation`] edges. Each edge is either
//! *forward* (authored by the user on this item) or *inverse* (`inverse: true`,
//! maintained by Kartoteka from some other item's forward edge). The governing invariant:
//! **the set of forward edges is the whole truth; every inverse edge is a function of it**,
//! so [`Library::fsck`](crate::Library::fsck) can regenerate all inverse edges from the
//! forward ones with no information loss.

use serde::{Deserialize, Serialize};

use crate::node::NodeType;

/// What kind of thing a relation `target` is, used only to curate which forward predicates
/// the UI offers first (see [`Predicate::forward_choices_for`]). An entry key resolves to
/// [`TargetKind::Work`]; a node slug maps from its [`NodeType`]. Purely advisory — it never
/// constrains what may be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Work,
    Person,
    School,
    Concept,
    Event,
    Place,
    /// A dangling target, or one whose kind is not (yet) known.
    Other,
}

impl From<NodeType> for TargetKind {
    fn from(t: NodeType) -> Self {
        match t {
            NodeType::Person => TargetKind::Person,
            NodeType::Concept => TargetKind::Concept,
            NodeType::School => TargetKind::School,
            NodeType::Event => TargetKind::Event,
            NodeType::Place => TargetKind::Place,
            // An uncatalogued work is still a work for predicate-offering purposes.
            NodeType::WorkUncataloged => TargetKind::Work,
        }
    }
}

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
    // Node-oriented predicates (M3, `docs/M3-SPEC.md` §2). These extend the same closed
    // vocabulary — there is no separate type for node relations.
    /// person/school → person/school.
    Influenced,
    InfluencedBy,
    /// person → entry (work).
    Authored,
    AuthoredBy,
    /// person → school.
    MemberOf,
    HasMember,
    /// entry/node → concept.
    About,
    DiscussedIn,
    /// event/place → event/place.
    PartOf,
    HasPart,
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
            Influenced => InfluencedBy,
            InfluencedBy => Influenced,
            Authored => AuthoredBy,
            AuthoredBy => Authored,
            MemberOf => HasMember,
            HasMember => MemberOf,
            About => DiscussedIn,
            DiscussedIn => About,
            PartOf => HasPart,
            HasPart => PartOf,
            Related => Related,
        }
    }

    /// True when the predicate is its own inverse (a symmetric relation, i.e. `Related`).
    pub fn is_self_inverse(self) -> bool {
        self.inverse() == self
    }

    /// The predicates a user picks when *authoring* a forward edge: the "primary direction"
    /// of each asymmetric pair plus the symmetric `Related`. The inverse halves
    /// (`CitedBy`, `HasCommentary`, `AuthoredBy`, …) are omitted because they are maintained
    /// automatically on the other endpoint — offering both directions would be redundant and
    /// confusing. Order is stable (used to build UI dropdowns): the M2 work-to-work
    /// predicates first, then the M3 node-oriented ones.
    pub fn forward_choices() -> [Predicate; 15] {
        use Predicate::*;
        [
            Related,
            Cites,
            Critiques,
            Reviews,
            CommentaryOn,
            TranslationOf,
            EditionOf,
            Supersedes,
            RepliesTo,
            Expands,
            Influenced,
            Authored,
            MemberOf,
            About,
            PartOf,
        ]
    }

    /// The forward predicates worth *offering first* when the edge points at a target of a
    /// given [`TargetKind`] (`docs/M3-SPEC.md` §2). Advisory UI curation only — the
    /// vocabulary stays fully open, so this never restricts what can be stored; it just
    /// surfaces the domain-appropriate predicates (e.g. `authored` when the target is a work,
    /// `member-of` when it is a school). `Related` always leads. An `Other`/unknown target
    /// falls back to the full [`forward_choices`](Self::forward_choices) list.
    pub fn forward_choices_for(target: TargetKind) -> Vec<Predicate> {
        use Predicate::*;
        let tail: &[Predicate] = match target {
            // A cited/critiqued/… target is a work; a person also "authored" it.
            TargetKind::Work => &[
                Cites,
                Critiques,
                Reviews,
                CommentaryOn,
                TranslationOf,
                EditionOf,
                Supersedes,
                RepliesTo,
                Expands,
                Authored,
            ],
            TargetKind::Person => &[Influenced],
            TargetKind::School => &[Influenced, MemberOf],
            TargetKind::Concept => &[About],
            TargetKind::Event | TargetKind::Place => &[PartOf],
            TargetKind::Other => {
                // Everything after the leading `Related` in the full list.
                return Self::forward_choices().to_vec();
            }
        };
        let mut out = Vec::with_capacity(tail.len() + 1);
        out.push(Related);
        out.extend_from_slice(tail);
        out
    }

    /// A human-readable label for UI display (e.g. `CitedBy` → "Cited by").
    pub fn label(self) -> &'static str {
        use Predicate::*;
        match self {
            Cites => "Cites",
            CitedBy => "Cited by",
            Critiques => "Critiques",
            CritiquedBy => "Critiqued by",
            Reviews => "Reviews",
            ReviewedBy => "Reviewed by",
            CommentaryOn => "Commentary on",
            HasCommentary => "Has commentary",
            TranslationOf => "Translation of",
            HasTranslation => "Has translation",
            EditionOf => "Edition of",
            HasEdition => "Has edition",
            Supersedes => "Supersedes",
            SupersededBy => "Superseded by",
            RepliesTo => "Replies to",
            RepliedToBy => "Replied to by",
            Expands => "Expands",
            ExpandedBy => "Expanded by",
            Influenced => "Influenced",
            InfluencedBy => "Influenced by",
            Authored => "Authored",
            AuthoredBy => "Authored by",
            MemberOf => "Member of",
            HasMember => "Has member",
            About => "About",
            DiscussedIn => "Discussed in",
            PartOf => "Part of",
            HasPart => "Has part",
            Related => "Related",
        }
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
            Predicate::Influenced,
            Predicate::InfluencedBy,
            Predicate::Authored,
            Predicate::AuthoredBy,
            Predicate::MemberOf,
            Predicate::HasMember,
            Predicate::About,
            Predicate::DiscussedIn,
            Predicate::PartOf,
            Predicate::HasPart,
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

    #[test]
    fn node_predicates_serialize_kebab() {
        for (p, kebab) in [
            (Predicate::Influenced, "influenced"),
            (Predicate::InfluencedBy, "influenced-by"),
            (Predicate::Authored, "authored"),
            (Predicate::AuthoredBy, "authored-by"),
            (Predicate::MemberOf, "member-of"),
            (Predicate::HasMember, "has-member"),
            (Predicate::About, "about"),
            (Predicate::DiscussedIn, "discussed-in"),
            (Predicate::PartOf, "part-of"),
            (Predicate::HasPart, "has-part"),
        ] {
            let r = Relation::forward(p, "x");
            let yaml = serde_yaml_ng::to_string(&r).unwrap();
            assert!(yaml.contains(&format!("predicate: {kebab}")), "{yaml}");
            // …and parses back to the same predicate.
            let back: Relation = serde_yaml_ng::from_str(&yaml).unwrap();
            assert_eq!(back.predicate, p);
        }
    }

    #[test]
    fn forward_choices_has_new_forwards_and_no_inverse_halves() {
        let fc = Predicate::forward_choices();
        for p in [
            Predicate::Influenced,
            Predicate::Authored,
            Predicate::MemberOf,
            Predicate::About,
            Predicate::PartOf,
        ] {
            assert!(fc.contains(&p), "{p:?} should be authorable");
        }
        // No inverse half is offered as a forward choice.
        for p in [
            Predicate::InfluencedBy,
            Predicate::AuthoredBy,
            Predicate::HasMember,
            Predicate::DiscussedIn,
            Predicate::HasPart,
        ] {
            assert!(!fc.contains(&p), "{p:?} is maintained, not authorable");
        }
    }

    #[test]
    fn forward_choices_for_curates_by_target_and_always_leads_with_related() {
        // A work target offers work-to-work predicates plus `authored`, never `about`.
        let work = Predicate::forward_choices_for(TargetKind::Work);
        assert_eq!(work.first(), Some(&Predicate::Related));
        assert!(work.contains(&Predicate::Cites));
        assert!(work.contains(&Predicate::Authored));
        assert!(!work.contains(&Predicate::About));

        // A school target offers membership + influence.
        let school = Predicate::forward_choices_for(TargetKind::School);
        assert!(school.contains(&Predicate::MemberOf));
        assert!(school.contains(&Predicate::Influenced));

        // A concept target offers `about`.
        assert!(Predicate::forward_choices_for(TargetKind::Concept).contains(&Predicate::About));

        // Event and place both offer `part-of`.
        assert!(Predicate::forward_choices_for(TargetKind::Event).contains(&Predicate::PartOf));
        assert!(Predicate::forward_choices_for(TargetKind::Place).contains(&Predicate::PartOf));

        // Every curated list is advisory but still a subset of the full authorable set.
        let full = Predicate::forward_choices();
        for kind in [
            TargetKind::Work,
            TargetKind::Person,
            TargetKind::School,
            TargetKind::Concept,
            TargetKind::Event,
            TargetKind::Place,
            TargetKind::Other,
        ] {
            for p in Predicate::forward_choices_for(kind) {
                assert!(full.contains(&p), "{p:?} offered for {kind:?} but not authorable");
            }
        }

        // Unknown target falls back to the full list.
        assert_eq!(
            Predicate::forward_choices_for(TargetKind::Other),
            full.to_vec()
        );
    }

    #[test]
    fn target_kind_maps_from_node_type() {
        assert_eq!(TargetKind::from(NodeType::Person), TargetKind::Person);
        assert_eq!(TargetKind::from(NodeType::School), TargetKind::School);
        assert_eq!(TargetKind::from(NodeType::Concept), TargetKind::Concept);
        assert_eq!(TargetKind::from(NodeType::Event), TargetKind::Event);
        assert_eq!(TargetKind::from(NodeType::Place), TargetKind::Place);
        // An uncatalogued work counts as a work for predicate offering.
        assert_eq!(TargetKind::from(NodeType::WorkUncataloged), TargetKind::Work);
    }
}
