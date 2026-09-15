//! Structured creators (author/editor/translator/…) for an entry, layered over Hayagriva's
//! own `author`/`editor`/`affiliated` fields — no Kartoteka-private YAML keys
//! (`docs/DATA-MODEL.md`). A `Creator` is one person (or organization) plus the role they
//! played; `parse_creators`/`write_creators` round-trip a flat, UI-ordered `Vec<Creator>`
//! against those three Hayagriva fields.

use hayagriva::types::{Person, PersonRole, PersonsWithRoles};
use hayagriva::Entry as HEntry;

/// The role a creator played, covering Hayagriva's top-level `author`/`editor` fields plus
/// every `affiliated`-list role it supports (`PersonRole`, kebab-case on disk). Hayagriva has
/// no "series editor" or generic "contributor" role, and its `PersonRole::Unknown(String)` is
/// `#[serde(skip)]` so it can't round-trip through YAML — neither is offered here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CreatorRole {
    Author,
    Editor,
    Translator,
    Foreword,
    Afterword,
    Introduction,
    Annotator,
    Commentator,
    Holder,
    Compiler,
    Illustrator,
    Narrator,
    Director,
    Cinematography,
    Composer,
    Producer,
    ExecutiveProducer,
    CastMember,
    Writer,
    Organizer,
    Founder,
    Collaborator,
}

impl CreatorRole {
    /// Every role, in the order offered in the type dropdown: `Author`/`Editor` first (by
    /// far the most common), then the rest grouped roughly by how often they come up in a
    /// personal library.
    pub const ALL: &'static [CreatorRole] = &[
        CreatorRole::Author,
        CreatorRole::Editor,
        CreatorRole::Translator,
        CreatorRole::Foreword,
        CreatorRole::Afterword,
        CreatorRole::Introduction,
        CreatorRole::Annotator,
        CreatorRole::Commentator,
        CreatorRole::Compiler,
        CreatorRole::Illustrator,
        CreatorRole::Narrator,
        CreatorRole::Director,
        CreatorRole::Cinematography,
        CreatorRole::Composer,
        CreatorRole::Producer,
        CreatorRole::ExecutiveProducer,
        CreatorRole::CastMember,
        CreatorRole::Writer,
        CreatorRole::Organizer,
        CreatorRole::Founder,
        CreatorRole::Collaborator,
        CreatorRole::Holder,
    ];

    /// Display label for the type dropdown.
    pub fn label(self) -> &'static str {
        match self {
            CreatorRole::Author => "Author",
            CreatorRole::Editor => "Editor",
            CreatorRole::Translator => "Translator",
            CreatorRole::Foreword => "Foreword",
            CreatorRole::Afterword => "Afterword",
            CreatorRole::Introduction => "Introduction",
            CreatorRole::Annotator => "Annotator",
            CreatorRole::Commentator => "Commentator",
            CreatorRole::Holder => "Rights Holder",
            CreatorRole::Compiler => "Compiler",
            CreatorRole::Illustrator => "Illustrator",
            CreatorRole::Narrator => "Narrator",
            CreatorRole::Director => "Director",
            CreatorRole::Cinematography => "Cinematography",
            CreatorRole::Composer => "Composer",
            CreatorRole::Producer => "Producer",
            CreatorRole::ExecutiveProducer => "Executive Producer",
            CreatorRole::CastMember => "Cast Member",
            CreatorRole::Writer => "Writer",
            CreatorRole::Organizer => "Organizer",
            CreatorRole::Founder => "Founder",
            CreatorRole::Collaborator => "Collaborator",
        }
    }

    /// The Hayagriva `affiliated` `PersonRole` this maps to, or `None` for `Author`/`Editor`
    /// (which live in their own top-level fields instead).
    fn person_role(self) -> Option<PersonRole> {
        Some(match self {
            CreatorRole::Author | CreatorRole::Editor => return None,
            CreatorRole::Translator => PersonRole::Translator,
            CreatorRole::Foreword => PersonRole::Foreword,
            CreatorRole::Afterword => PersonRole::Afterword,
            CreatorRole::Introduction => PersonRole::Introduction,
            CreatorRole::Annotator => PersonRole::Annotator,
            CreatorRole::Commentator => PersonRole::Commentator,
            CreatorRole::Holder => PersonRole::Holder,
            CreatorRole::Compiler => PersonRole::Compiler,
            CreatorRole::Illustrator => PersonRole::Illustrator,
            CreatorRole::Narrator => PersonRole::Narrator,
            CreatorRole::Director => PersonRole::Director,
            CreatorRole::Cinematography => PersonRole::Cinematography,
            CreatorRole::Composer => PersonRole::Composer,
            CreatorRole::Producer => PersonRole::Producer,
            CreatorRole::ExecutiveProducer => PersonRole::ExecutiveProducer,
            CreatorRole::CastMember => PersonRole::CastMember,
            CreatorRole::Writer => PersonRole::Writer,
            CreatorRole::Organizer => PersonRole::Organizer,
            CreatorRole::Founder => PersonRole::Founder,
            CreatorRole::Collaborator => PersonRole::Collaborator,
        })
    }

    /// The exact string this role is written as on disk: `"author"`/`"editor"` for those two
    /// (top-level YAML fields), or the kebab-case `PersonRole` name otherwise (matching
    /// Hayagriva's own `#[serde(rename_all = "kebab-case")]`). Useful to callers building
    /// YAML by hand rather than through [`write_creators`] (e.g. a plain-text entry
    /// template).
    pub fn role_key(self) -> &'static str {
        match self {
            CreatorRole::Author => "author",
            CreatorRole::Editor => "editor",
            CreatorRole::Translator => "translator",
            CreatorRole::Foreword => "foreword",
            CreatorRole::Afterword => "afterword",
            CreatorRole::Introduction => "introduction",
            CreatorRole::Annotator => "annotator",
            CreatorRole::Commentator => "commentator",
            CreatorRole::Holder => "holder",
            CreatorRole::Compiler => "compiler",
            CreatorRole::Illustrator => "illustrator",
            CreatorRole::Narrator => "narrator",
            CreatorRole::Director => "director",
            CreatorRole::Cinematography => "cinematography",
            CreatorRole::Composer => "composer",
            CreatorRole::Producer => "producer",
            CreatorRole::ExecutiveProducer => "executive-producer",
            CreatorRole::CastMember => "cast-member",
            CreatorRole::Writer => "writer",
            CreatorRole::Organizer => "organizer",
            CreatorRole::Founder => "founder",
            CreatorRole::Collaborator => "collaborator",
        }
    }

    fn from_person_role(role: &PersonRole) -> Option<Self> {
        Some(match role {
            PersonRole::Translator => CreatorRole::Translator,
            PersonRole::Foreword => CreatorRole::Foreword,
            PersonRole::Afterword => CreatorRole::Afterword,
            PersonRole::Introduction => CreatorRole::Introduction,
            PersonRole::Annotator => CreatorRole::Annotator,
            PersonRole::Commentator => CreatorRole::Commentator,
            PersonRole::Holder => CreatorRole::Holder,
            PersonRole::Compiler => CreatorRole::Compiler,
            PersonRole::Illustrator => CreatorRole::Illustrator,
            PersonRole::Narrator => CreatorRole::Narrator,
            PersonRole::Director => CreatorRole::Director,
            PersonRole::Cinematography => CreatorRole::Cinematography,
            PersonRole::Composer => CreatorRole::Composer,
            PersonRole::Producer => CreatorRole::Producer,
            PersonRole::ExecutiveProducer => CreatorRole::ExecutiveProducer,
            PersonRole::CastMember => CreatorRole::CastMember,
            PersonRole::Writer => CreatorRole::Writer,
            PersonRole::Organizer => CreatorRole::Organizer,
            PersonRole::Founder => CreatorRole::Founder,
            PersonRole::Collaborator => CreatorRole::Collaborator,
            // Catches `Unknown` (can't be produced by Hayagriva's own YAML deserializer — it's
            // `#[serde(skip)]` — but guard anyway) and any role Hayagriva adds later that we
            // don't yet know how to offer in the type dropdown (`PersonRole` is
            // `#[non_exhaustive]`).
            _ => return None,
        })
    }
}

/// One creator: a person (or organization, in single-field mode) plus the role they played.
///
/// `single_field` means the whole name lives in `family` and `given` is ignored on write —
/// this mirrors Hayagriva's own convention where a comma-free scalar string becomes a
/// family-only `Person`, so it's exactly how an organization name (or a mononym) already
/// round-trips. `prefix`/`suffix`/`comma_suffix`/`alias` come from `hayagriva::types::Person`
/// and are preserved from existing data (e.g. a "van"/"Jr." parsed from a prior BibLaTeX
/// import) but aren't exposed in the editor UI yet — carried through opaquely so nothing is
/// lost on save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Creator {
    pub role: CreatorRole,
    pub family: String,
    pub given: String,
    pub single_field: bool,
    pub prefix: Option<String>,
    pub suffix: Option<String>,
    pub comma_suffix: bool,
    pub alias: Option<String>,
}

impl Creator {
    /// A plain two-field creator with no prefix/suffix/alias.
    pub fn new(role: CreatorRole, family: impl Into<String>, given: impl Into<String>) -> Self {
        let given = given.into();
        Creator {
            role,
            family: family.into(),
            single_field: given.trim().is_empty(),
            given,
            prefix: None,
            suffix: None,
            comma_suffix: false,
            alias: None,
        }
    }

    /// A single-field creator (organization, mononym, …).
    pub fn new_single_field(role: CreatorRole, name: impl Into<String>) -> Self {
        Creator {
            role,
            family: name.into(),
            given: String::new(),
            single_field: true,
            prefix: None,
            suffix: None,
            comma_suffix: false,
            alias: None,
        }
    }

    fn from_person(role: CreatorRole, p: &Person) -> Self {
        let given = p.given_name.clone().unwrap_or_default();
        Creator {
            role,
            family: p.name.clone(),
            single_field: given.trim().is_empty(),
            given,
            prefix: p.prefix.clone(),
            suffix: p.suffix.clone(),
            comma_suffix: p.comma_suffix,
            alias: p.alias.clone(),
        }
    }

    fn to_person(&self) -> Person {
        let given_name = if self.single_field {
            None
        } else {
            let g = self.given.trim();
            if g.is_empty() {
                None
            } else {
                Some(g.to_string())
            }
        };
        Person {
            name: self.family.trim().to_string(),
            given_name,
            prefix: self.prefix.clone(),
            suffix: self.suffix.clone(),
            comma_suffix: self.comma_suffix,
            alias: self.alias.clone(),
        }
    }

    /// Canonical `"Family, Given"` (or bare `"Family"`) display line, for list/search display.
    pub fn display_line(&self) -> String {
        if self.single_field || self.given.trim().is_empty() {
            self.family.trim().to_string()
        } else {
            format!("{}, {}", self.family.trim(), self.given.trim())
        }
    }

    /// Build a creator from free-form imported text with no user in the loop to correct a
    /// bad guess (EPUB/OpenLibrary metadata): `"Family, Given"` if there's a comma (matching
    /// Hayagriva's own `Person::from_strings` convention); otherwise split naturally on the
    /// last whitespace (last token = family, e.g. `"Desmond Lee"` → family `Lee`, given
    /// `Desmond`); a single word becomes a single-field name (`"Plato"`).
    pub fn from_natural_text(role: CreatorRole, text: &str) -> Self {
        let text = text.trim();
        if let Some((family, given)) = text.split_once(',') {
            let given = given.trim();
            return if given.is_empty() {
                Creator::new_single_field(role, family.trim())
            } else {
                Creator::new(role, family.trim(), given)
            };
        }
        match text.rsplit_once(char::is_whitespace) {
            Some((given, family)) if !family.trim().is_empty() => {
                Creator::new(role, family.trim(), given.trim())
            }
            _ => Creator::new_single_field(role, text),
        }
    }
}

/// Read every creator off an entry, in a stable, meaningful order: all `author:` entries
/// first, then all `editor:` entries, then each `affiliated:` group (translator, compiler,
/// …) in the order Hayagriva stored them, people within a group in their stored order.
pub fn parse_creators(entry: &HEntry) -> Vec<Creator> {
    let mut out = Vec::new();
    if let Some(people) = entry.authors() {
        out.extend(
            people
                .iter()
                .map(|p| Creator::from_person(CreatorRole::Author, p)),
        );
    }
    if let Some(people) = entry.editors() {
        out.extend(
            people
                .iter()
                .map(|p| Creator::from_person(CreatorRole::Editor, p)),
        );
    }
    if let Some(groups) = entry.affiliated() {
        for group in groups {
            let Some(role) = CreatorRole::from_person_role(&group.role) else {
                continue;
            };
            out.extend(group.names.iter().map(|p| Creator::from_person(role, p)));
        }
    }
    out
}

/// Partition a flat, UI-ordered creator list into Hayagriva's three fields, preserving
/// relative order within each. Creators sharing a non-author/editor role — even if they
/// aren't contiguous in `creators` — merge into a single `affiliated` group for that role,
/// in first-occurrence order, so e.g. two translators listed with an editor between them
/// still write as one `role: translator` group with both names.
pub fn write_creators(creators: &[Creator]) -> (Vec<Person>, Vec<Person>, Vec<PersonsWithRoles>) {
    let mut authors = Vec::new();
    let mut editors = Vec::new();
    let mut affiliated: Vec<PersonsWithRoles> = Vec::new();

    for c in creators {
        match c.role {
            CreatorRole::Author => authors.push(c.to_person()),
            CreatorRole::Editor => editors.push(c.to_person()),
            other => {
                let role = other
                    .person_role()
                    .expect("non-author/editor CreatorRole always maps to a PersonRole");
                match affiliated.iter_mut().find(|g| g.role == role) {
                    Some(group) => group.names.push(c.to_person()),
                    None => affiliated.push(PersonsWithRoles::new(vec![c.to_person()], role)),
                }
            }
        }
    }

    (authors, editors, affiliated)
}

/// The sort/citation-key family name: the first `author`, else the first `editor`, else the
/// first person in the first `affiliated` group — matching how CSL styles themselves already
/// substitute a missing author (author → editor → translator …). Satisfies "first author is
/// the sort author" exactly whenever the entry has an author, and degrades sensibly for an
/// editor-only or translator-only entry.
pub fn sort_family_name(entry: &HEntry) -> Option<String> {
    let creators = parse_creators(entry);
    creators
        .iter()
        .find(|c| c.role == CreatorRole::Author)
        .or_else(|| creators.iter().find(|c| c.role == CreatorRole::Editor))
        .or_else(|| creators.first())
        .map(|c| c.family.clone())
}

/// All creators sharing the "primary" role — the same one `sort_family_name` would pick —
/// joined as a single display string (`"Cone, James H., Doe, Jane"`), for indexing/search.
/// Empty if the entry has no creators at all.
pub fn display_names(entry: &HEntry) -> String {
    let creators = parse_creators(entry);
    let Some(primary) = creators.first().map(|c| c.role) else {
        return String::new();
    };
    creators
        .iter()
        .filter(|c| c.role == primary)
        .map(Creator::display_line)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::parse_single;
    use std::path::Path;

    fn parse(yaml: &str) -> HEntry {
        parse_single(yaml, Path::new("t.yml")).unwrap().entry
    }

    #[test]
    fn round_trips_author_editor_and_affiliated() {
        let yaml = "k:\n  type: book\n  title: T\n  author:\n    - Doe, Jane\n  editor:\n    - Roe, Rick\n  affiliated:\n    - names:\n        - Lee, Desmond\n      role: translator\n";
        let entry = parse(yaml);
        let creators = parse_creators(&entry);
        assert_eq!(creators.len(), 3);
        assert_eq!(creators[0].role, CreatorRole::Author);
        assert_eq!(creators[0].family, "Doe");
        assert_eq!(creators[1].role, CreatorRole::Editor);
        assert_eq!(creators[2].role, CreatorRole::Translator);
        assert_eq!(creators[2].family, "Lee");

        let (authors, editors, affiliated) = write_creators(&creators);
        assert_eq!(authors.len(), 1);
        assert_eq!(editors.len(), 1);
        assert_eq!(affiliated.len(), 1);
        assert_eq!(affiliated[0].role, PersonRole::Translator);
    }

    #[test]
    fn sort_family_name_falls_back_author_then_editor_then_affiliated() {
        assert_eq!(
            sort_family_name(&parse("k:\n  type: book\n  title: T\n  author:\n    - Doe, Jane\n  editor:\n    - Roe, Rick\n")),
            Some("Doe".to_string())
        );
        assert_eq!(
            sort_family_name(&parse(
                "k:\n  type: book\n  title: T\n  editor:\n    - Roe, Rick\n"
            )),
            Some("Roe".to_string())
        );
        assert_eq!(
            sort_family_name(&parse("k:\n  type: book\n  title: T\n  affiliated:\n    - names:\n        - Lee, Desmond\n      role: translator\n")),
            Some("Lee".to_string())
        );
        assert_eq!(
            sort_family_name(&parse("k:\n  type: book\n  title: T\n")),
            None
        );
    }

    #[test]
    fn single_field_creator_has_no_comma_on_write() {
        let creators = vec![Creator::new_single_field(CreatorRole::Author, "UNESCO")];
        let (authors, _, _) = write_creators(&creators);
        assert_eq!(authors[0].name, "UNESCO");
        assert!(authors[0].given_name.is_none());
    }

    #[test]
    fn from_natural_text_splits_on_comma_then_last_whitespace_then_single_field() {
        let c = Creator::from_natural_text(CreatorRole::Author, "Doe, Jane");
        assert_eq!(
            (c.family.as_str(), c.given.as_str(), c.single_field),
            ("Doe", "Jane", false)
        );

        let c = Creator::from_natural_text(CreatorRole::Author, "Desmond Lee");
        assert_eq!(
            (c.family.as_str(), c.given.as_str(), c.single_field),
            ("Lee", "Desmond", false)
        );

        let c = Creator::from_natural_text(CreatorRole::Author, "Plato");
        assert_eq!(
            (c.family.as_str(), c.given.as_str(), c.single_field),
            ("Plato", "", true)
        );
    }

    #[test]
    fn preserves_prefix_and_suffix_through_parse_and_write() {
        let yaml = "k:\n  type: book\n  title: T\n  author:\n    - name: Beethoven\n      given-name: Ludwig van\n      prefix: van\n      suffix: Jr.\n";
        let entry = parse(yaml);
        let creators = parse_creators(&entry);
        assert_eq!(creators[0].prefix.as_deref(), Some("van"));
        assert_eq!(creators[0].suffix.as_deref(), Some("Jr."));

        let (authors, _, _) = write_creators(&creators);
        assert_eq!(authors[0].prefix.as_deref(), Some("van"));
        assert_eq!(authors[0].suffix.as_deref(), Some("Jr."));
    }

    #[test]
    fn role_key_matches_hayagriva_serde_kebab_case() {
        for role in CreatorRole::ALL {
            if let Some(person_role) = role.person_role() {
                let serialized = serde_yaml_ng::to_value(&person_role).unwrap();
                assert_eq!(
                    serialized.as_str(),
                    Some(role.role_key()),
                    "role_key() for {role:?} must match Hayagriva's own kebab-case name"
                );
            }
        }
    }

    // Guards the round-trip path `entry.rs` uses when writing creators back into a full
    // document (via `serde_yaml_ng::to_value` on `Person`/`PersonsWithRoles`).
    #[test]
    fn hayagriva_person_serializes_to_a_value_directly() {
        let p = Person {
            name: "Doe".to_string(),
            given_name: Some("Jane".to_string()),
            prefix: None,
            suffix: None,
            comma_suffix: false,
            alias: None,
        };
        let v = serde_yaml_ng::to_value(&p).unwrap();
        assert_eq!(v.as_str(), Some("Doe, Jane"));
    }
}
