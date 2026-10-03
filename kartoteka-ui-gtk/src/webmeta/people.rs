//! Who is talking in an episode, read from its title and description when the page doesn't say
//! in structured data (CBC, and most broadcasters, don't). This is a guess, and callers say so:
//! the added entry is opened for checking.
//!
//! - The **host** is a name right after a cue like "host", "interviewed by" or "conversation
//!   with" (CBC Ideas: "…in this conversation with Nahlah Ayed", "…she told host Nahlah Ayed").
//! - The **guest** is a person's name in the episode title that the description also mentions
//!   ("Empire of AI: Tech journalist Karen Hao") — and isn't the host.

/// Cues after which a name is the host/interviewer. The "conversation with" family can name
/// either side, so those only count when the name found isn't the guest.
const HOST_CUES: &[&str] = &[
    "hosted by ",
    "co-host ",
    "guest host ",
    "host ",
    "interviewed by ",
    "interviewer ",
    "conversation with ",
    "conversations with ",
    "speaks with ",
    "spoke with ",
    "talks with ",
    "talked with ",
    "sits down with ",
    "sat down with ",
];

/// Capitalized words that open a phrase rather than a name ("The Vanishing Forests").
const NOT_NAME_START: &[&str] = &[
    "The", "A", "An", "In", "On", "Of", "Why", "How", "What", "When", "Where", "Who", "Our", "My",
    "Your", "This", "That", "These", "Those", "Is", "Are", "Can", "Should", "Inside", "Episode",
    "Part", "Podcast", "Live",
];

/// The people in an episode: who interviews and who is interviewed. Either may be empty.
#[derive(Debug, Default, PartialEq)]
pub struct People {
    pub host: String,
    pub guests: Vec<String>,
}

/// Guess the host and guests from an episode's `title` and `text` (its description and any
/// other prose). `exclude` are names that are never people here (the show, the network).
pub fn guess(title: &str, text: &str, exclude: &[&str]) -> People {
    let excluded = |n: &str| exclude.iter().any(|e| e.eq_ignore_ascii_case(n));
    let guests: Vec<String> = names(title)
        .into_iter()
        .filter(|n| !excluded(n) && text.contains(n.as_str()))
        .collect();

    let mut host = String::new();
    for cue in HOST_CUES {
        let weak = cue.contains("with");
        let mut from = 0;
        while let Some(rel) = find_ci(&text[from..], cue) {
            let at = from + rel + cue.len();
            from = at;
            let Some(name) = names(&text[at..]).into_iter().next() else {
                continue;
            };
            // The name must start right at the cue, not further along the sentence.
            if !text[at..].trim_start().starts_with(name.as_str()) {
                continue;
            }
            if excluded(&name) || (weak && guests.contains(&name)) {
                continue;
            }
            host = name;
            break;
        }
        if !host.is_empty() {
            break;
        }
    }
    let guests = guests.into_iter().filter(|g| *g != host).collect();
    People { host, guests }
}

/// Runs of two or three capitalized words ("Karen Hao", "Mary Jo Smith") — the shape of a
/// person's name in running text. A capitalized phrase that opens with a word like "The" is
/// skipped whole, so "The Vanishing Forests" yields nothing.
fn names(text: &str) -> Vec<String> {
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '(' | ')' | '"'))
        .collect();
    let clean = |raw: &'_ str| -> String {
        let w = raw.trim_end_matches(['.', '!', '?', '\'', '’', '”']);
        w.strip_suffix("'s")
            .or_else(|| w.strip_suffix("’s"))
            .unwrap_or(w)
            .to_string()
    };
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if NOT_NAME_START.contains(&clean(words[i]).as_str()) {
            i += 1;
            while i < words.len() && is_name_word(&clean(words[i])) {
                i += 1;
            }
            continue;
        }
        let mut run = Vec::new();
        let mut j = i;
        while j < words.len() && run.len() < 3 {
            let w = clean(words[j]);
            if !is_name_word(&w) {
                break;
            }
            let ends_here = w != words[j]; // punctuation or a possessive closes the name
            run.push(w);
            j += 1;
            if ends_here {
                break;
            }
        }
        if run.len() >= 2 {
            let name = run.join(" ");
            if !out.contains(&name) {
                out.push(name);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// "Karen", "O'Brien", "Saint-Exupéry" — capitalized, then at least one lowercase letter.
fn is_name_word(w: &str) -> bool {
    let mut chars = w.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_uppercase()
        && w.chars().skip(1).any(char::is_lowercase)
        && w.chars()
            .all(|c| c.is_alphabetic() || matches!(c, '-' | '\'' | '’' | '.'))
}

/// Case-insensitive `find` for an ASCII needle, at the start of a word ("host" not "ghost").
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let lower = haystack.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(needle) {
        let at = from + rel;
        if !lower[..at].chars().last().is_some_and(char::is_alphabetic) {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbc_ideas_episode() {
        let title = "Empire of AI: Tech journalist Karen Hao";
        let text = "Tech journalist Karen Hao investigated OpenAI for her bestseller, Empire of \
                    AI. Hao clearly sets out the urgent stakes, in this conversation with Nahlah \
                    Ayed, alongside a March 2026 talk, recorded at the University of Toronto.";
        let p = guess(title, text, &["Ideas", "CBC Radio"]);
        assert_eq!(p.host, "Nahlah Ayed");
        assert_eq!(p.guests, vec!["Karen Hao"]);
    }

    #[test]
    fn host_cue_beats_a_guest_named_after_with() {
        let p = guess(
            "Jane Guest on rivers",
            "Host Sam Talker in conversation with Jane Guest about rivers.",
            &[],
        );
        assert_eq!(p.host, "Sam Talker");
        assert_eq!(p.guests, vec!["Jane Guest"]);
    }

    #[test]
    fn a_title_phrase_is_not_a_guest() {
        let p = guess(
            "The Vanishing Forests of Old Canada",
            "The Vanishing Forests are explored by host Ann Bell.",
            &[],
        );
        assert_eq!(p.host, "Ann Bell");
        assert!(p.guests.is_empty(), "{:?}", p.guests);
    }

    #[test]
    fn nothing_to_find() {
        assert_eq!(guess("A talk", "About things.", &[]), People::default());
    }
}
