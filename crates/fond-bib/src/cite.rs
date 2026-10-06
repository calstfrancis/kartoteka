//! Typst citation text; implemented in `fond-annot` so Pereplyot produces exactly the same.

pub use fond_annot::cite::*;

#[cfg(test)]
mod tests {
    #[test]
    fn the_citation_is_found_again_by_the_usage_scanner() {
        let text = super::typst_citation("cone1970black", Some("12-14"));
        assert_eq!(
            crate::project::scan_typst_citation_keys(&text),
            vec!["cone1970black"]
        );
    }
}
