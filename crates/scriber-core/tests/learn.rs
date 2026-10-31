//! Learning vocabulary from the user's corrections.

#[cfg(test)]
mod tests {
    use scriber_core::learn::{
        MAX_LEARNED, MAX_RUN_WORDS, MAX_RUNS, MIN_FIX_CHARS, candidates, enforce, learn, remember,
    };

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    /// (list, learned, vocabulary) => (new list, added).
    type RememberCase = (
        &'static [&'static str],
        &'static [&'static str],
        &'static [&'static str],
        &'static [&'static str],
        &'static [&'static str],
    );

    const SIX: &str = "Use TightBeam and Scribe and Github and Svelt and Ollama and Cargo now.";

    /// Original => edited => what `learn` returns.
    const CORPUS: &[(&str, &str, &[&str])] = &[
        ("Open TightBeam now.", "Open tightbeam now.", &["tightbeam"]),
        (
            "The tight beam runner.",
            "The tightbeam runner.",
            &["tightbeam"],
        ),
        ("Ask the scribe app.", "Ask the Scriber app.", &["Scriber"]),
        ("The build was fast.", "The build was quick.", &[]),
        ("The cat sat.", "the cat sat.", &[]),
        ("TightBeam's queue.", "tightbeam's queue.", &["tightbeam"]),
        (
            SIX,
            "Use tightbeam and Scriber and GitHub and Svelte and ollama and cargo now.",
            &[],
        ),
        (
            SIX,
            "Use tightbeam and Scriber and GitHub and Svelte and ollama and Cargo now.",
            &["tightbeam", "Scriber", "GitHub", "Svelte", "ollama"],
        ),
        ("Open the app.", "Open the new app.", &[]),
        ("Open the old app.", "Open the app.", &[]),
        ("Run tightbeam now.", "Run tight beam now.", &["tight beam"]),
        (
            "The tight-beam runner.",
            "The tightbeam runner.",
            &["tightbeam"],
        ),
        ("Check the g t d board.", "Check the GTD board.", &["GTD"]),
        (
            "Say alpha beta gamma delta now.",
            "Say one two three four now.",
            &[],
        ),
        ("Send it to Tom.", "Send it to Tim.", &[]),
        ("A bird flew.", "A word flew.", &[]),
        ("Ping Ollamma now.", "Ping Ollama now.", &["Ollama"]),
        ("Done? Yes.", "Done? yes.", &[]),
        ("Hello there.", "hello there.", &[]),
        ("Done. Hello.", "Done. hello.", &[]),
        (
            "Open TightBeam and TightBeam.",
            "Open tightbeam and Tightbeam.",
            &["tightbeam"],
        ),
        ("Ask Zoë now.", "Ask Zoe now.", &[]),
        ("Open Münchenn now.", "Open München now.", &["München"]),
        ("Same text.", "Same text.", &[]),
    ];

    #[test]
    fn the_limits_are_pinned() {
        assert_eq!(
            (MAX_RUN_WORDS, MAX_RUNS, MIN_FIX_CHARS, MAX_LEARNED),
            (3, 5, 4, 100),
            "limits"
        );
    }

    #[test]
    fn the_corpus_learns_what_it_should() {
        for (original, edited, expected) in CORPUS {
            assert_eq!(
                learn(original, edited),
                strings(expected),
                "{original} => {edited}"
            );
        }
    }

    #[test]
    fn six_fixes_are_all_candidates_but_nothing_is_learned() {
        let edited = "Use tightbeam and Scriber and GitHub and Svelte and ollama and cargo now.";
        assert_eq!(
            candidates(SIX, edited),
            strings(&[
                "tightbeam",
                "Scriber",
                "GitHub",
                "Svelte",
                "ollama",
                "cargo"
            ]),
            "candidates"
        );
        assert_eq!(
            candidates(
                "Open TightBeam and TightBeam.",
                "Open tightbeam and Tightbeam."
            ),
            strings(&["tightbeam", "Tightbeam"]),
            "candidates are not de-duplicated"
        );
    }

    #[test]
    fn edits_far_apart_and_quoted_sentence_ends_are_aligned() {
        assert_eq!(
            learn(
                "Ask scribe about it. \"Done.\" Github is up and it is fast",
                "Ask Scriber about it. \"Done.\" github is up and it is quick"
            ),
            strings(&["Scriber"]),
            "a capital after a quoted sentence end is a sentence start; fast to quick is a style edit"
        );
        assert_eq!(
            learn("one two three", "one two three four five"),
            strings(&[]),
            "an insertion at the end"
        );
        assert_eq!(
            learn("x tightbeam y z", "x y tight beam z"),
            strings(&[]),
            "a moved word is an insertion and a deletion"
        );
        assert_eq!(
            learn("a b c", "c b a"),
            strings(&[]),
            "a reordering is a deletion and an insertion"
        );
        assert_eq!(
            learn("it's fine", "Its fine"),
            strings(&[]),
            "it's reads as it, and Its is too short a fix"
        );
        assert_eq!(
            learn("New Scribe!", "New Scriber!"),
            strings(&["Scriber"]),
            "punctuation is stripped"
        );
        assert_eq!(
            learn("\u{130}stanbul now.", "i\u{307}stanbul now."),
            strings(&["i\u{307}stanbul"]),
            "a case change that changes the length qualifies"
        );
    }

    #[test]
    fn remember_puts_new_terms_first() {
        let cases: &[RememberCase] = &[
            (&[], &["tightbeam"], &[], &["tightbeam"], &["tightbeam"]),
            (
                &["Scriber", "TightBeam"],
                &["tightbeam"],
                &[],
                &["tightbeam", "Scriber"],
                &["tightbeam"],
            ),
            (
                &["Scriber", "tightbeam"],
                &["tightbeam"],
                &[],
                &["tightbeam", "Scriber"],
                &["tightbeam"],
            ),
            (&[], &["tightbeam"], &["tightbeam"], &[], &[]),
            (
                &[],
                &["tightbeam"],
                &["TightBeam"],
                &["tightbeam"],
                &["tightbeam"],
            ),
        ];
        for (list, learned, vocabulary, new, added) in cases {
            assert_eq!(
                remember(&strings(list), &strings(learned), &strings(vocabulary)),
                (strings(new), strings(added)),
                "{list:?} + {learned:?}"
            );
        }
        for size in [100, 99] {
            let list: Vec<String> = (1..=size).map(|n| format!("t{n}")).collect();
            let (new, added) = remember(&list, &strings(&["new"]), &[]);
            assert_eq!(new.len(), 100, "{size}: capped");
            assert_eq!(
                new.first().map(String::as_str),
                Some("new"),
                "{size}: first"
            );
            assert_eq!(new.last().map(String::as_str), Some("t99"), "{size}: last");
            assert_eq!(added, strings(&["new"]), "{size}: added");
        }
    }

    #[test]
    fn enforce_rewrites_learned_spellings() {
        let cases: &[(&[&str], &str, &str)] = &[
            (
                &["tightbeam"],
                "Open TightBeam, then tight beam and Tight-Beam.",
                "Open tightbeam, then tightbeam and tightbeam.",
            ),
            (&["tightbeam"], "TightBeam's queue", "tightbeam's queue"),
            (
                &["tightbeam"],
                "tightbeams and xtightbeam",
                "tightbeams and xtightbeam",
            ),
            (&["tightbeam"], "foo-tightbeam", "foo-tightbeam"),
            (&["tightbeam"], "Tightbeam works.", "tightbeam works."),
            (&["tight beam"], "tightbeam", "tight beam"),
            (&["Scriber"], "the scriber app", "the Scriber app"),
            (&["v2.0"], "V2.0", "V2.0"),
            (&[], "Open TightBeam.", "Open TightBeam."),
            (&["München"], "in münchen today", "in München today"),
            (
                &["tightbeam"],
                "tightbeam_x and x_tightbeam",
                "tightbeam_x and x_tightbeam",
            ),
            (&["tightbeam"], "tight  beam", "tight  beam"),
            (&["tightbeam"], "tight beam-", "tight beam-"),
            (&["GTD"], "the g t d board", "the GTD board"),
            (&["ab"], "a b c d", "ab c d"),
            (&["abcd"], "a b c d", "a b c d"),
            (&["", "-"], "a - b", "a - b"),
            (&["tight beam", "TightBeam"], "TIGHTBEAM", "tight beam"),
            (&["Scrib", "Scriber"], "scriber scrib", "Scriber Scrib"),
            (&["ab", "abc"], "a b c", "abc"),
        ];
        for (terms, text, expected) in cases {
            assert_eq!(
                enforce(text, &strings(terms)),
                *expected,
                "{terms:?}: {text}"
            );
        }
    }
}
