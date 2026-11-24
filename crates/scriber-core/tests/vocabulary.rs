//! The transcriber prompt.

#[cfg(test)]
mod tests {
    use scriber_core::vocabulary::{
        MAX_PROMPT_CHARS, PREFIX, SEPARATOR, STYLE, glossary, prompt, prompt_for,
    };

    #[test]
    fn the_format_is_pinned() {
        assert_eq!(
            STYLE, "Here's what I need. First, check the board; then, once it's done, let me know.",
            "style"
        );
        assert_eq!(STYLE.chars().count(), 78, "style length");
        assert_eq!(PREFIX, "Glossary: ", "prefix");
        assert_eq!(SEPARATOR, ", ", "separator");
        assert_eq!(MAX_PROMPT_CHARS, 500, "limit");
    }

    #[test]
    fn terms_are_listed_in_file_order() {
        assert_eq!(
            prompt("talos\nagent-gtd\nPersonal KB\n"),
            "Here's what I need. First, check the board; then, once it's done, let me know. Glossary: talos, agent-gtd, Personal KB.",
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
        assert_eq!(fits, 410, "room for terms");
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
        assert_eq!(rest, 407, "room after the first term");
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

    #[test]
    fn terms_are_cleaned_like_the_prompt() {
        assert_eq!(
            scriber_core::vocabulary::terms("  Claude \n# c\n\nAgent\u{7}GTD\n"),
            vec!["Claude".to_string(), "AgentGTD".to_string()],
            "trimmed, comments and blanks skipped, control characters removed"
        );
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn learned_terms_come_first_in_the_glossary() {
        assert_eq!(
            glossary(&strings(&["tightbeam"]), &strings(&["Rust", "TightBeam"])),
            strings(&["tightbeam", "Rust"]),
            "learned first, case-insensitive duplicates dropped"
        );
        assert_eq!(
            glossary(&[], &strings(&["Rust"])),
            strings(&["Rust"]),
            "vocabulary alone"
        );
        assert_eq!(prompt_for(&[]), prompt(""), "no terms");
        assert_eq!(
            prompt_for(&strings(&["a", "b"])),
            prompt("a\nb\n"),
            "the same rule"
        );
    }
}

#[cfg(test)]
mod app_tests {
    #[test]
    fn app_terms_share_is_a_prefix_and_user_duplicates_keep_their_spelling() {
        use scriber_core::app_terms::AppTerm;
        use scriber_core::vocabulary::{prompt_for, prompt_with_app};
        let app = |term: String| AppTerm {
            term,
            spoken: Some("never spoken here".into()),
        };
        let short: Vec<_> = (0..25).map(|i| app(format!("t{i}"))).collect();
        let written: Vec<_> = short.iter().map(|a| a.term.clone()).collect();
        assert_eq!(prompt_with_app(&short, &[]), prompt_for(&written));
        assert_eq!(prompt_with_app(&[], &written), prompt_for(&written));
        let boundary = vec![
            app("a".repeat(64)),
            app("b".repeat(64)),
            app("c".repeat(64)),
            app("dd".into()),
        ];
        let boundary_user = "u".repeat(208);
        let prompt = prompt_with_app(&boundary, std::slice::from_ref(&boundary_user));
        assert_eq!(prompt.chars().count(), 500);
        assert!(prompt.contains(&boundary_user));
        assert!(!prompt.contains("never spoken"));
        assert!(prompt_with_app(&boundary, &["u".repeat(209)]).ends_with("dd."));
        let overflow = vec![
            app("a".repeat(64)),
            app("b".repeat(64)),
            app("c".repeat(64)),
            app("ddd".into()),
            app("z".into()),
        ];
        assert!(prompt_with_app(&overflow, &[]).ends_with(&format!("{}.", "c".repeat(64))));
        for count in [40, 60] {
            let mut user: Vec<_> = (0..count).map(|i| format!("user{i:08}")).collect();
            user[count - 1] = "vortac".into();
            let duplicate_prompt = prompt_with_app(&[app("Vortac".into())], &user);
            assert_eq!(duplicate_prompt.matches("vortac").count(), 1);
            assert!(duplicate_prompt.contains("Glossary: vortac, user00000000"));
        }
        assert!(
            prompt_with_app(&[app("a".repeat(201)), app("z".into())], &["User".into()])
                .ends_with("Glossary: User.")
        );
    }
}
