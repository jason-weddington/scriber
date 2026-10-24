//! The glossary prompt.

#[cfg(test)]
mod tests {
    use scriber_core::vocabulary::{MAX_PROMPT_CHARS, PREFIX, SEPARATOR, prompt};

    #[test]
    fn the_format_is_pinned() {
        assert_eq!(PREFIX, "Glossary: ", "prefix");
        assert_eq!(SEPARATOR, ", ", "separator");
        assert_eq!(MAX_PROMPT_CHARS, 500, "limit");
    }

    #[test]
    fn terms_are_listed_in_file_order() {
        assert_eq!(
            prompt("talos\nagent-gtd\nPersonal KB\n").as_deref(),
            Some("Glossary: talos, agent-gtd, Personal KB"),
            "three terms"
        );
        assert_eq!(
            prompt("one").as_deref(),
            Some("Glossary: one"),
            "one term, no separator"
        );
    }

    #[test]
    fn blanks_comments_and_whitespace_are_ignored() {
        let file = "# my terms\n\n  talos  \r\n   # indented comment\n\tC#\n";
        assert_eq!(
            prompt(file).as_deref(),
            Some("Glossary: talos, C#"),
            "trimmed, comments gone"
        );
    }

    #[test]
    fn no_terms_is_no_prompt() {
        assert_eq!(prompt(""), None, "empty file");
        assert_eq!(
            prompt("# only a comment\n\n   \n"),
            None,
            "nothing but comments and blanks"
        );
    }

    #[test]
    fn control_characters_are_removed_and_a_term_of_nothing_else_is_skipped() {
        assert_eq!(
            prompt("a\u{0}b\n\u{7}\nc").as_deref(),
            Some("Glossary: ab, c"),
            "stripped"
        );
    }

    #[test]
    fn the_prompt_stops_at_the_limit() {
        let fits = "x".repeat(MAX_PROMPT_CHARS - PREFIX.len());
        let want = format!("{PREFIX}{fits}");
        assert_eq!(
            prompt(&fits).as_deref(),
            Some(want.as_str()),
            "exactly at the limit"
        );
        assert_eq!(prompt(&format!("{fits}y")), None, "one over");
    }

    #[test]
    fn the_separator_counts_and_the_first_term_that_overflows_ends_the_list() {
        // "Glossary: a" is 11 characters; ", " and 487 more make 500.
        let exact = "b".repeat(487);
        let full = format!("{PREFIX}a{SEPARATOR}{exact}");
        assert_eq!(
            prompt(&format!("a\n{exact}")).as_deref(),
            Some(full.as_str()),
            "fits exactly"
        );
        let over = "b".repeat(488);
        assert_eq!(
            prompt(&format!("a\n{over}\nz")).as_deref(),
            Some("Glossary: a"),
            "the overflowing term and everything after it are left out"
        );
    }

    #[test]
    fn the_limit_counts_characters_not_bytes() {
        let accents = "é".repeat(MAX_PROMPT_CHARS - PREFIX.len());
        assert!(prompt(&accents).is_some(), "490 characters, 980 bytes");
    }
}
