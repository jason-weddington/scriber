//! The transcriber prompt.

#[cfg(test)]
mod tests {
    use scriber_core::vocabulary::{MAX_PROMPT_CHARS, PREFIX, SEPARATOR, STYLE, prompt};

    #[test]
    fn the_format_is_pinned() {
        assert_eq!(
            STYLE,
            "Okay, here's what I need. First, check the board; then, once it's done, let me know.",
            "style"
        );
        assert_eq!(STYLE.chars().count(), 84, "style length");
        assert_eq!(PREFIX, "Glossary: ", "prefix");
        assert_eq!(SEPARATOR, ", ", "separator");
        assert_eq!(MAX_PROMPT_CHARS, 500, "limit");
    }

    #[test]
    fn terms_are_listed_in_file_order() {
        assert_eq!(
            prompt("talos\nagent-gtd\nPersonal KB\n"),
            "Okay, here's what I need. First, check the board; then, once it's done, let me know. Glossary: talos, agent-gtd, Personal KB.",
            "three terms"
        );
        assert_eq!(
            prompt("one"),
            format!("{STYLE} Glossary: one."),
            "one term, no separator"
        );
    }

    #[test]
    fn blanks_comments_and_whitespace_are_ignored() {
        let file = "# my terms\n\n  talos  \r\n   # indented comment\n\tC#\n";
        assert_eq!(
            prompt(file),
            format!("{STYLE} Glossary: talos, C#."),
            "trimmed, comments gone"
        );
    }

    #[test]
    fn no_terms_gives_the_style_sentence_alone() {
        assert_eq!(prompt(""), STYLE, "empty file");
        assert_eq!(
            prompt("# only a comment\n\n   \n"),
            STYLE,
            "nothing but comments and blanks"
        );
        assert_eq!(prompt("\u{7}\n\u{0}\n"), STYLE, "only control characters");
    }

    #[test]
    fn control_characters_are_removed_and_a_term_of_nothing_else_is_skipped() {
        assert_eq!(
            prompt("a\u{0}b\n\u{7}\nc"),
            format!("{STYLE} Glossary: ab, c."),
            "stripped"
        );
    }

    #[test]
    fn a_term_ending_in_a_period_is_not_deduplicated() {
        assert_eq!(
            prompt("Inc."),
            format!("{STYLE} Glossary: Inc.."),
            "period appended regardless"
        );
    }

    #[test]
    fn the_prompt_stops_at_the_limit() {
        let fits = MAX_PROMPT_CHARS - STYLE.chars().count() - 1 - PREFIX.len() - 1;
        assert_eq!(fits, 404, "room for terms");
        let x = "x".repeat(fits);
        let got = prompt(&x);
        assert_eq!(got, format!("{STYLE} {PREFIX}{x}."), "exactly at the limit");
        assert_eq!(got.chars().count(), 500, "500 characters");
        assert_eq!(prompt(&"x".repeat(fits + 1)), STYLE, "one over");
    }

    #[test]
    fn the_separator_counts_and_the_first_term_that_overflows_ends_the_list() {
        let fits = MAX_PROMPT_CHARS - STYLE.chars().count() - 1 - PREFIX.len() - 1;
        let rest = fits - 1 - SEPARATOR.len();
        assert_eq!(rest, 401, "room after the first term");
        let b = "b".repeat(rest);
        let got = prompt(&format!("a\n{b}"));
        assert_eq!(
            got,
            format!("{STYLE} {PREFIX}a{SEPARATOR}{b}."),
            "fits exactly"
        );
        assert_eq!(got.chars().count(), 500, "500 characters");
        assert_eq!(
            prompt(&format!("a\n{}\nz", "b".repeat(rest + 1))),
            format!("{STYLE} {PREFIX}a."),
            "the overflowing term and everything after it are left out"
        );
    }

    #[test]
    fn the_limit_counts_characters_not_bytes() {
        let fits = MAX_PROMPT_CHARS - STYLE.chars().count() - 1 - PREFIX.len() - 1;
        let got = prompt(&"\u{e9}".repeat(fits));
        assert_ne!(got, STYLE, "kept");
        assert_eq!(got.chars().count(), 500, "500 characters");
        assert!(got.len() > 500, "more bytes than characters");
    }
}
