//! Learning vocabulary from the user's corrections.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use scriber_core::learn::{
        MAX_LEARNED, MAX_RUN_WORDS, MAX_RUNS, MIN_FIX_CHARS, candidates, candidates_with_lexicon,
        enforce, learn, learn_with_lexicon, remember,
    };
    use scriber_core::ports::Lexicon;

    struct FakeLexicon(BTreeSet<String>);

    impl FakeLexicon {
        fn new(words: &[&str]) -> Self {
            Self(words.iter().map(|w| w.to_lowercase()).collect())
        }
    }

    impl Lexicon for FakeLexicon {
        fn is_word(&self, word: &str) -> bool {
            self.0.contains(word)
        }
    }

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
        ("Open ATLAS now.", "Open Atlas now.", &["Atlas"]),
        ("The beem broke.", "The beam broke.", &["beam"]),
        (
            "Ask Kuburnates now.",
            "Ask Kubernetes now.",
            &["Kubernetes"],
        ),
        ("Say abcd now.", "Say a b c d now.", &[]),
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
            (&["TightBeam"], "retightbeam now", "retightbeam now"),
            (&["Tight Beam"], "tightbeam now", "Tight Beam now"),
        ];
        for (terms, text, expected) in cases {
            assert_eq!(
                enforce(text, &strings(terms)),
                *expected,
                "{terms:?}: {text}"
            );
        }
    }

    #[test]
    fn lexicon_vetoes_ordinary_singletons_and_keeps_intentional_structural_and_multiword() {
        let lex = FakeLexicon::new(&[
            "lead", "leads", "their", "there", "effect", "affect", "tested", "testing", "deploy",
            "deployed", "swift", "iphone", "notebook", "email", "team",
        ]);
        let some: Option<&dyn Lexicon> = Some(&lex);

        // Ordinary singleton vetoes that qualify without a lexicon:
        // lowercase, sentence-initial title case, and straight/curly possessives.
        let vetoed: &[(&str, &str)] = &[
            ("Check leads now.", "Check lead's now."),
            ("Leads are here.", "Lead\u{2019}s are here."),
            ("Leads are here.", "Lead's are here."),
            ("It will affect us.", "It will effect us."),
            ("We deployed it.", "We deploy it."),
            ("Ask the Lead now.", "Ask the lead now."),
            ("Swyft works here.", "Swift works here."),
        ];
        for (original, edited) in vetoed {
            assert_eq!(
                learn_with_lexicon(original, edited, some),
                Vec::<String>::new(),
                "vetoed with lexicon: {original} => {edited}"
            );
            assert!(
                !learn(original, edited).is_empty(),
                "lexicon-absent keeps eligible edit: {original} => {edited}"
            );
            assert_eq!(
                learn_with_lexicon(original, edited, None),
                learn(original, edited),
                "None matches lexicon-free learn: {original} => {edited}"
            );
        }
        for (original, edited) in [
            ("Check there now.", "Check their now."),
            ("Keep testing now.", "Keep tested now."),
        ] {
            assert_eq!(
                learn_with_lexicon(original, edited, some),
                Vec::<String>::new(),
                "not learned with lexicon: {original} => {edited}"
            );
        }

        // Retained eligible corrections with lexicon attached.
        let kept: &[(&str, &str, &[&str])] = &[
            ("Ask swift now.", "Ask Swift now.", &["Swift"]),
            ("Ask swyft now.", "Ask Swift now.", &["Swift"]),
            ("Open iphone now.", "Open iPhone now.", &["iPhone"]),
            ("Ipone is here.", "iPhone is here.", &["iPhone"]),
            ("Leed is here.", "LEAD is here.", &["LEAD"]),
            ("Ask scribe now.", "Ask Scriber now.", &["Scriber"]),
            ("Scribe is here.", "Scriber is here.", &["Scriber"]),
            ("The scribe works.", "The scriber works.", &["scriber"]),
            ("Open note book now.", "Open notebook now.", &["notebook"]),
            ("Open notebook now.", "Open note book now.", &["note book"]),
            ("Send e-mail now.", "Send email now.", &["email"]),
            (
                "The tight-beam runner.",
                "The tightbeam runner.",
                &["tightbeam"],
            ),
            (
                "Visit lead team now.",
                "Visit Lead Team now.",
                &["Lead Team"],
            ),
        ];
        for (original, edited, expected) in kept {
            assert_eq!(
                learn_with_lexicon(original, edited, some),
                strings(expected),
                "retained with lexicon: {original} => {edited}"
            );
        }
    }

    #[test]
    fn lexicon_applies_before_the_five_occurrence_cap_and_deduplication() {
        let lex = FakeLexicon::new(&["lead", "cargo"]);
        let some: Option<&dyn Lexicon> = Some(&lex);

        // Six repeated eligible runs still hit MAX_RUNS=5 before deduplication.
        let six_orig =
            "Use TightBeam and TightBeam and TightBeam and TightBeam and TightBeam and TightBeam.";
        let six_edit =
            "Use tightbeam and tightbeam and tightbeam and tightbeam and tightbeam and tightbeam.";
        assert_eq!(
            candidates_with_lexicon(six_orig, six_edit, some).len(),
            6,
            "uncapped, undeduplicated"
        );
        assert_eq!(
            learn_with_lexicon(six_orig, six_edit, some),
            Vec::<String>::new(),
            "six repeated eligible runs cap before dedup"
        );

        // Six qualifying runs where one is vetoed becomes five and is learned.
        let edited = "Use tightbeam and Scriber and GitHub and Svelte and ollama and cargo now.";
        assert_eq!(
            learn(SIX, edited),
            Vec::<String>::new(),
            "without lexicon, six runs cap to empty"
        );
        assert_eq!(
            candidates_with_lexicon(SIX, edited, some),
            strings(&["tightbeam", "Scriber", "GitHub", "Svelte", "ollama"]),
            "one ordinary word vetoed leaves five"
        );
        assert_eq!(
            learn_with_lexicon(SIX, edited, some),
            strings(&["tightbeam", "Scriber", "GitHub", "Svelte", "ollama"]),
            "five surviving runs are learned"
        );
    }
}
