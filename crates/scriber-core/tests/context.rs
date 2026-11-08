//! The context extraction rules: how a turn is tokenized, what is discarded,
//! what is kept, and how the terms are deduped, ordered and truncated.

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use scriber_core::context::{MAX_TERMS, terms};

    fn strings(turns: &[&str]) -> Vec<String> {
        turns.iter().map(|s| (*s).to_string()).collect()
    }

    /// [`terms`] over `turns` with a lexicon that knows no word, which leaves keep
    /// rules (a)-(d) alone.
    fn kept(turns: &[&str]) -> Vec<String> {
        terms(&strings(turns), Some(&|_: &str| false), MAX_TERMS)
    }

    #[test]
    fn the_context_term_budget_is_thirty() {
        assert_eq!(scriber_core::context::MAX_TERMS, 30, "context terms");
        assert_eq!(MAX_TERMS, 30, "the constant");
    }

    #[test]
    fn a_turn_is_split_on_whitespace_and_backticks_and_trimmed_at_its_edges() {
        assert_eq!(
            terms(
                &["run `kb_search` and @tightbeam".to_string()],
                Some(&|word: &str| ["run", "and"].contains(&word)),
                MAX_TERMS
            ),
            ["kb_search", "tightbeam"],
            "the pieces keep `kb_search` (rule c) and `tightbeam` (rule e)"
        );
    }

    #[test]
    fn a_piece_that_starts_with_a_tilde_is_dropped_before_trimming() {
        assert_eq!(kept(&["~/notes.md"]), Vec::<String>::new(), "a path");
        assert_eq!(kept(&["~"]), Vec::<String>::new(), "bare ~");
        assert_eq!(
            kept(&["~ StreamRecorder"]),
            ["StreamRecorder"],
            "the other piece survives"
        );
    }

    #[test]
    fn a_slash_discards_a_token() {
        assert_eq!(
            kept(&["https://example.com"]),
            Vec::<String>::new(),
            "a URL"
        );
        assert_eq!(kept(&["src/lib/bridge.ts"]), Vec::<String>::new(), "a path");
    }

    #[test]
    fn a_www_prefix_discards_a_token() {
        assert_eq!(kept(&["www.example.com"]), Vec::<String>::new(), "a host");
    }

    #[test]
    fn a_backslash_discards_a_token() {
        assert_eq!(
            kept(&["C:\\StreamRecorder"]),
            Vec::<String>::new(),
            "a Windows path"
        );
    }

    #[test]
    fn a_token_of_digits_and_separators_alone_is_discarded() {
        for token in ["9:43", "9.43", "1,000.50", "2026-11-08"] {
            assert_eq!(kept(&[token]), Vec::<String>::new(), "{token}");
        }
    }

    #[test]
    fn a_token_with_a_long_hex_run_is_discarded() {
        assert_eq!(kept(&["f82e306a"]), Vec::<String>::new(), "a hash");
        assert_eq!(
            kept(&["tsk_01a1b2c3"]),
            Vec::<String>::new(),
            "an id with a hash in it"
        );
    }

    #[test]
    fn a_token_longer_than_forty_characters_is_discarded() {
        let long = format!("{}2", "a1".repeat(20));
        assert_eq!(long.len(), 41, "41 characters");
        assert_eq!(kept(&[&long]), Vec::<String>::new(), "too long");
    }

    #[test]
    fn both_boundaries_are_kept() {
        assert_eq!(
            terms(&["f82e306".to_string()], Some(&|_: &str| false), MAX_TERMS),
            ["f82e306"],
            "seven hex characters"
        );
        let forty = "a1".repeat(20);
        assert_eq!(forty.len(), 40, "40 characters");
        assert_eq!(
            terms(
                std::slice::from_ref(&forty),
                Some(&|_: &str| false),
                MAX_TERMS
            ),
            [forty],
            "exactly forty characters"
        );
    }

    #[test]
    fn a_one_character_token_is_kept_by_no_rule() {
        assert_eq!(kept(&["a"]), Vec::<String>::new(), "too short and no rule");
    }

    #[test]
    fn a_lowercase_followed_by_an_uppercase_keeps_a_token() {
        let known = ["streamrecorder", "iphone"];
        let ask = |word: &str| known.contains(&word);
        let is_word: Option<&dyn Fn(&str) -> bool> = Some(&ask);
        assert_eq!(
            terms(&["StreamRecorder iPhone".to_string()], is_word, MAX_TERMS),
            ["StreamRecorder", "iPhone"],
            "camel case needs no lexicon"
        );
        assert_eq!(
            terms(&["streamrecorder".to_string()], is_word, MAX_TERMS),
            Vec::<String>::new(),
            "the plain word is known"
        );
    }

    #[test]
    fn a_digit_and_a_letter_keeps_a_token() {
        assert_eq!(kept(&["qwen3.8 v4"]), ["qwen3.8", "v4"], "versions");
    }

    #[test]
    fn an_inner_separator_between_alphanumerics_keeps_a_token() {
        assert_eq!(
            terms(
                &strings(&["scriber-core kb_search node.js"]),
                Some(&|_: &str| true),
                MAX_TERMS
            ),
            ["scriber-core", "kb_search", "node.js"],
            "the separators"
        );
        assert_eq!(
            terms(&strings(&["a--b"]), Some(&|_: &str| true), MAX_TERMS),
            Vec::<String>::new(),
            "a double separator has a non-alphanumeric neighbour"
        );
        assert_eq!(
            terms(&strings(&["core-"]), Some(&|_: &str| false), MAX_TERMS),
            ["core"],
            "the trailing separator was trimmed"
        );
    }

    #[test]
    fn a_short_all_caps_acronym_keeps_a_token() {
        assert_eq!(
            terms(
                &strings(&["GTD RCA GTDS"]),
                Some(&|_: &str| true),
                MAX_TERMS
            ),
            ["GTD", "RCA", "GTDS"],
            "two to six capitals"
        );
        assert_eq!(
            terms(&strings(&["GTDSXZQ Gtd"]), Some(&|_: &str| true), MAX_TERMS),
            Vec::<String>::new(),
            "too long, and not all caps"
        );
    }

    #[test]
    fn the_lexicon_branch_needs_a_lexicon_and_three_letters() {
        assert_eq!(
            terms(&strings(&["yukara"]), Some(&|_: &str| false), MAX_TERMS),
            ["yukara"],
            "an unknown word"
        );
        assert_eq!(
            terms(&strings(&["yukara"]), Some(&|_: &str| true), MAX_TERMS),
            Vec::<String>::new(),
            "a known word"
        );
        assert_eq!(
            terms(&strings(&["yukara"]), None, MAX_TERMS),
            Vec::<String>::new(),
            "no lexicon disables the rule"
        );
        assert_eq!(
            terms(&strings(&["StreamRecorder"]), None, MAX_TERMS),
            ["StreamRecorder"],
            "rule (a) needs no lexicon"
        );
    }

    #[test]
    fn unicode_letters_are_alphabetic() {
        assert_eq!(
            terms(&strings(&["naïve"]), Some(&|_: &str| false), MAX_TERMS),
            ["naïve"],
            "a lowercase word"
        );
        assert_eq!(
            terms(&strings(&["Ñandú"]), Some(&|_: &str| false), MAX_TERMS),
            ["Ñandú"],
            "a capitalised word"
        );
    }

    #[test]
    fn the_lexicon_is_asked_only_for_an_unmatched_alphabetic_token_lowercased() {
        let asked = RefCell::new(Vec::new());
        let ask = |word: &str| {
            asked.borrow_mut().push(word.to_string());
            false
        };
        let _first = terms(
            &strings(&["Gtd streamrecorder a--b"]),
            Some(&ask),
            MAX_TERMS,
        );
        assert_eq!(*asked.borrow(), ["gtd", "streamrecorder"], "asked");

        let later = RefCell::new(Vec::new());
        let ask_later = |word: &str| {
            later.borrow_mut().push(word.to_string());
            false
        };
        let _second = terms(
            &strings(&["StreamRecorder kb_search"]),
            Some(&ask_later),
            MAX_TERMS,
        );
        assert_eq!(*later.borrow(), Vec::<String>::new(), "both kept earlier");
    }

    #[test]
    fn the_first_spelling_wins_and_every_occurrence_counts() {
        assert_eq!(
            kept(&["StreamRecorder streamrecorder"]),
            ["StreamRecorder"],
            "one key, the first spelling"
        );
    }

    #[test]
    fn terms_are_ordered_by_frequency_then_recency_then_first_appearance() {
        assert_eq!(kept(&["Alpha Beta"]), ["Alpha", "Beta"], "first appearance");
        assert_eq!(kept(&["Alpha", "Beta"]), ["Beta", "Alpha"], "most recent");
        assert_eq!(
            kept(&["Alpha Beta", "Beta"]),
            ["Beta", "Alpha"],
            "frequency first"
        );
        assert_eq!(
            terms(&strings(&["Alpha Beta Gamma"]), Some(&|_: &str| false), 2),
            ["Alpha", "Beta"],
            "truncated to max"
        );
        assert_eq!(kept(&[]), Vec::<String>::new(), "no turns");
        assert_eq!(kept(&[""]), Vec::<String>::new(), "an empty turn");
        assert_eq!(
            terms(&strings(&["Alpha"]), Some(&|_: &str| false), 0),
            Vec::<String>::new(),
            "no room"
        );
    }

    #[test]
    fn without_a_lexicon_only_the_shape_rules_keep_a_token() {
        assert_eq!(
            terms(&strings(&["Alpha Beta"]), None, MAX_TERMS),
            Vec::<String>::new(),
            "ordinary words"
        );
        assert_eq!(
            terms(&strings(&["Gamma"]), None, MAX_TERMS),
            Vec::<String>::new(),
            "one word"
        );
    }
}
