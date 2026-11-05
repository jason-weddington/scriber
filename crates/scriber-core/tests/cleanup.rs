//! The cleanup pass's request and the sanity check on the model's reply.

#[cfg(test)]
mod tests {
    use scriber_core::cleanup::{
        CLEANED, FAILED, MAX_TERMS, MIN_WORDS_FOR_RATIO, NEW_WORDS_MIN, NEW_WORDS_PER, OFF, PROMPT,
        Reject, Replacement, Reply, UNCHANGED, accept, accept_reply, filter, learned_terms,
        request, spelled,
    };

    const RAW30: &str = "Tell the team I mean, I mean, what they really meant was, but what I actually meant was the deploy moves to Wednesday and the release notes go out Thursday.";

    const CLAWED: &str = "Ask Clawed to check the board.";

    const TWENTY: &str = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty.";

    fn terms(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| (*t).to_string()).collect()
    }

    #[test]
    fn the_constants_are_pinned() {
        const S1: &str = "You correct a dictated transcript. Make only four kinds of change.";
        const S2: &str = "First, where a word or phrase was misheard and sounds closely like one of the glossary terms (and fits the sentence), replace it with that term spelled exactly as in the glossary; never replace an unrelated name or foreign word just because it starts with a similar sound.";
        const S3: &str = "Second, where the speaker explicitly spells out a word or name inside the transcript (for example, \"X is spelled A-B-C\"), use that spelling in normal title case (not all-caps unless it is an acronym) everywhere that word or name appears in the transcript (fixing any phonetic mishearing, including when joined to another word), while keeping the speaker's spelling sentence intact unless they were correcting a false start.";
        const S4: &str = "Third, where the speaker corrected themselves while speaking (a false start, a repeated phrase, or words such as \"I mean\" or \"what I actually meant was\"), work out what they were trying to say. If their final restatement says it cleanly, keep that restatement exactly as spoken. If it does not, write what they were trying to say in words that match the rest of the transcript, its vocabulary and its tone, adding as few words they did not say as you can. Either way, delete the abandoned words.";
        const S5: &str = "Fourth, where a clock time followed by a.m. or p.m. has a dot between the hour and the minutes, change the dot to a colon: 9.43 a.m. becomes 9:43 a.m. Leave every other number unchanged, including version numbers and decimals.";
        const S6: &str = "Change nothing else: keep every other word, the word order, the spelling, the punctuation and the paragraph breaks exactly as they are. Do not rephrase, summarize, add, explain or answer anything else in the transcript.";
        const S7: &str = "Record every change you make (or any spelled-out word, with `from` and `to` equal when already spelled right) in `replacements`, every new word or proper name the speaker spelled out in `learned` in normal title case, and the corrected transcript in `transcript` (unchanged when nothing needs changing).";
        assert_eq!(MAX_TERMS, 200, "terms");
        assert_eq!(MIN_WORDS_FOR_RATIO, 20, "ratio floor");
        assert_eq!(NEW_WORDS_MIN, 3, "new words min");
        assert_eq!(NEW_WORDS_PER, 20, "new words per");
        assert_eq!(
            [OFF, CLEANED, UNCHANGED, FAILED],
            ["off", "cleaned", "unchanged", "failed"],
            "labels"
        );
        assert_eq!(PROMPT, [S1, S2, S3, S4, S5, S6, S7].join(" "), "prompt");
        assert!(
            PROMPT
                .starts_with("You correct a dictated transcript. Make only four kinds of change."),
            "opens with rule one"
        );
        assert!(!PROMPT.contains("three kinds"), "four rules");
        assert!(
            !PROMPT.contains("Reply with the corrected transcript only"),
            "no closing reply sentence"
        );
    }

    #[test]
    fn the_reply_shape_is_pinned() {
        assert_eq!(
            Reply::new("a"),
            Reply {
                replacements: vec![],
                learned: vec![],
                transcript: "a".to_string(),
            },
            "new"
        );
        assert_eq!(Reply::default(), Reply::new(""), "default");
        assert_eq!(
            serde_json::from_str::<Reply>(r#"{"transcript":"t"}"#).unwrap(),
            Reply::new("t"),
            "absent arrays"
        );
        assert_eq!(
            serde_json::to_string(&Reply::new("t")).unwrap(),
            r#"{"replacements":[],"learned":[],"transcript":"t"}"#,
            "order"
        );
        let full = Reply {
            replacements: vec![Replacement {
                from: "Clawed".to_string(),
                to: "Claude".to_string(),
            }],
            learned: vec!["Claude".to_string()],
            transcript: "Ask Claude to check the board.".to_string(),
        };
        assert_eq!(
            serde_json::from_str::<Reply>(&serde_json::to_string(&full).unwrap()).unwrap(),
            full,
            "round trip"
        );
    }

    #[test]
    fn spelled_finds_spelled_out_words() {
        assert_eq!(spelled("Y-A-G-U-R-A."), ["yagura"], "one");
        assert_eq!(
            spelled("spell it A-I-J-U and the A-I-J-U again"),
            ["aiju"],
            "de-duplicated"
        );
        assert_eq!(spelled("A-B"), Vec::<String>::new(), "two parts");
        assert_eq!(
            spelled("a-1-c"),
            Vec::<String>::new(),
            "a part that is not a letter"
        );
        assert_eq!(spelled("X-Y-Z-th"), Vec::<String>::new(), "a long part");
        assert_eq!(spelled("Zed"), Vec::<String>::new(), "no dashes");
        assert_eq!(spelled(""), Vec::<String>::new(), "empty");
    }

    fn replacement(from: &str, to: &str) -> Replacement {
        Replacement {
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    fn reply_with(replacements: Vec<Replacement>, transcript: &str) -> Reply {
        Reply {
            replacements,
            learned: vec![],
            transcript: transcript.to_string(),
        }
    }

    #[test]
    fn filter_leaves_long_or_numeric_replacements_alone() {
        let raw = "The deploy moves to Wednesday.";
        for from_to in [
            ("the deploy moves to Wednesday", "Deploy is Wednesday"),
            ("two", "2"),
            ("", "x"),
        ] {
            let reply = reply_with(vec![replacement(from_to.0, from_to.1)], raw);
            assert_eq!(filter(raw, &reply), reply, "{from_to:?}");
        }
    }

    #[test]
    fn filter_reverts_a_spelled_out_capitalisation() {
        let spelled_raw = "I want CLAUDE spelled C-L-A-U-D-E.";
        let spelled_reply = reply_with(vec![replacement("Claude", "CLAUDE")], spelled_raw);
        assert_eq!(
            filter(spelled_raw, &spelled_reply).transcript,
            "I want Claude spelled C-L-A-U-D-E.",
            "spelled out"
        );

        let plain_raw = "I want Claude on the board.";
        let plain_reply = reply_with(
            vec![replacement("Claude", "CLAUDE")],
            "I want CLAUDE on the board.",
        );
        let filtered = filter(plain_raw, &plain_reply);
        assert_eq!(filtered, plain_reply, "not spelled out");
        assert_eq!(
            filtered.transcript, "I want CLAUDE on the board.",
            "unchanged"
        );

        let short_raw = "I filed it in Gtd, spelled G-T-D.";
        let short_reply = reply_with(vec![replacement("Gtd", "GTD")], short_raw);
        assert_eq!(
            filter(short_raw, &short_reply),
            short_reply,
            "three letters"
        );

        let caps_raw = "I want CLAUDE spelled C-L-A-U-D-E.";
        let caps_reply = reply_with(vec![replacement("CLAUDE", "CLAUDE")], caps_raw);
        assert_eq!(filter(caps_raw, &caps_reply), caps_reply, "already caps");
    }

    #[test]
    fn filter_reverts_a_distant_respelling() {
        let raw = "Ask Clawed to check the board.";
        let distant = reply_with(vec![replacement("Yukara", "Chie")], raw);
        assert_eq!(
            filter(raw, &distant).replacements,
            Vec::<Replacement>::new(),
            "distant"
        );
        assert_eq!(filter(raw, &distant).transcript, raw, "reverted");

        let threshold = reply_with(vec![replacement("Yagura", "YAGNI")], raw);
        assert_eq!(
            filter(raw, &threshold).replacements,
            Vec::<Replacement>::new(),
            "threshold"
        );

        let close = reply_with(vec![replacement("Talus", "Talos")], raw);
        assert_eq!(filter(raw, &close), close, "close");

        let boundary = reply_with(vec![replacement("Talus", "Tolas")], raw);
        assert_eq!(filter(raw, &boundary), boundary, "at 40% boundary");

        let spelled_raw = "I want Yagura spelled Y-A-G-U-R-A.";
        let spelled_reply = reply_with(vec![replacement("Yagura", "Yakura")], spelled_raw);
        assert_eq!(
            filter(spelled_raw, &spelled_reply).replacements,
            Vec::<Replacement>::new(),
            "spelled word"
        );

        let grounded_raw = "Spell it A-I-J-U.";
        let grounded_reply = reply_with(vec![replacement("Clawed", "Aijulabs")], grounded_raw);
        assert_eq!(
            filter(grounded_raw, &grounded_reply),
            grounded_reply,
            "grounded"
        );
    }

    #[test]
    fn filter_reverts_by_replacing_the_reply_word() {
        let twice_raw = "Yukara met Yukara at nine.";
        let twice = Reply {
            replacements: vec![replacement("Yukara", "Chie"), replacement("Yukara", "Chie")],
            learned: vec!["Kept".to_string()],
            transcript: "Chie met Chie at nine.".to_string(),
        };
        assert_eq!(
            filter(twice_raw, &twice),
            Reply {
                replacements: vec![],
                learned: vec!["Kept".to_string()],
                transcript: "Yukara met Yukara at nine.".to_string(),
            },
            "progressive"
        );

        let mixed_raw = "Yukara says Talus is up.";
        let mixed = Reply {
            replacements: vec![replacement("Yukara", "Chie"), replacement("Talus", "Talos")],
            learned: vec!["Aiju".to_string()],
            transcript: "Chie says Talos is up.".to_string(),
        };
        assert_eq!(
            filter(mixed_raw, &mixed),
            Reply {
                replacements: vec![replacement("Talus", "Talos")],
                learned: vec!["Aiju".to_string()],
                transcript: "Yukara says Talos is up.".to_string(),
            },
            "good kept in place"
        );
    }

    #[test]
    fn learned_terms_keeps_what_the_speaker_spelled() {
        let raw = "I want A-I-J and A-I-J-U on the board.";
        let reply = Reply {
            replacements: vec![],
            learned: strings(&[
                "Aij",
                "Aiju Labs Extra Words",
                "Aiju.Core",
                "Aiju!",
                "",
                "Aiju-Core",
            ]),
            transcript:
                "I want Aij and Aiju and Aiju Labs Extra Words and Aiju.Core and Aiju-Core on the board."
                    .to_string(),
        };
        assert_eq!(
            learned_terms(raw, &reply, &[]),
            ["Aiju", "Aiju-Core"],
            "kept"
        );
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn learned_terms_titles_and_expands() {
        let caps_raw = "I want A-I-J-U on the board.";
        let caps = Reply {
            replacements: vec![],
            learned: strings(&["AIJU"]),
            transcript: "I want Aiju on the board.".to_string(),
        };
        assert_eq!(learned_terms(caps_raw, &caps, &[]), ["Aiju"], "titled");

        let labs_raw = "Use A-I-J-U Labs on the board.";
        let joined = Reply {
            replacements: vec![],
            learned: strings(&["AijuLabs"]),
            transcript: "Use Aiju Labs on the board.".to_string(),
        };
        assert_eq!(
            learned_terms(labs_raw, &joined, &[]),
            strings(&["Aiju", "Aiju Labs"]),
            "expanded"
        );

        let shouted = Reply {
            replacements: vec![],
            learned: strings(&["AIJULABS"]),
            transcript: "Use Aiju Labs on the board.".to_string(),
        };
        assert_eq!(
            learned_terms(labs_raw, &shouted, &[]),
            strings(&["Aiju", "Aiju Labs"]),
            "expanded from caps"
        );
    }

    #[test]
    fn learned_terms_applies_its_keep_rules() {
        let raw = "I want A-I-J-U on the board.";
        let transcript = "I want Aiju on the board.";
        let with = |learned: &[&str]| {
            let reply = Reply {
                replacements: vec![],
                learned: strings(learned),
                transcript: transcript.to_string(),
            };
            learned_terms(raw, &reply, &[])
        };
        assert_eq!(
            learned_terms(
                raw,
                &Reply {
                    replacements: vec![],
                    learned: strings(&["Aiju"]),
                    transcript: transcript.to_string(),
                },
                &["aiju".to_string()]
            ),
            Vec::<String>::new(),
            "already a term"
        );
        assert_eq!(with(&["Zed"]), Vec::<String>::new(), "grounded nowhere");
        assert_eq!(with(&["Aiju", "Board"]), ["Aiju"], "board not grounded");
        assert_eq!(with(&["Aiju", "aiju"]), ["Aiju"], "case-insensitive");
        assert_eq!(
            with(&["Aiju", "Zedx", "Aiju Labs"]),
            ["Aiju"],
            "ungrounded and multi-word"
        );

        let many_raw = "I want A-I-J-U on the board.";
        let five = Reply {
            replacements: vec![],
            learned: strings(&["Aiju", "Aijua", "Aijub", "Aijuc", "Aijud"]),
            transcript: "Aiju Aijua Aijub Aijuc Aijud".to_string(),
        };
        assert_eq!(
            learned_terms(many_raw, &five, &[]),
            strings(&["Aiju", "Aijua", "Aijub", "Aijuc", "Aijud"]),
            "five allowed"
        );
        let many = Reply {
            replacements: vec![],
            learned: strings(&["Aiju", "Aijua", "Aijub", "Aijuc", "Aijud", "Aijue"]),
            transcript: "Aiju Aijua Aijub Aijuc Aijud Aijue".to_string(),
        };
        assert_eq!(
            learned_terms(many_raw, &many, &[]),
            Vec::<String>::new(),
            "capped"
        );

        let yagura_raw = "The Yagura release is late.";
        let yagura = Reply {
            replacements: vec![replacement("Yakura", "Yagura")],
            learned: strings(&["Yagura"]),
            transcript: yagura_raw.to_string(),
        };
        assert_eq!(
            learned_terms(yagura_raw, &yagura, &[]),
            ["Yagura"],
            "from a replacement"
        );

        let clawed_raw = "Ask Clawed to check the board.";
        let clawed = Reply {
            replacements: vec![replacement("Clawed", "Claude")],
            learned: strings(&["Claude"]),
            transcript: "Ask Claude to check the board.".to_string(),
        };
        assert_eq!(
            learned_terms(clawed_raw, &clawed, &[]),
            Vec::<String>::new(),
            "too distant"
        );
    }

    #[test]
    fn accept_reply_judges_the_filtered_transcript() {
        let raw = "Ask Clawed to check the board.";
        let reply = reply_with(
            vec![replacement("Clawed", "Chie")],
            "Ask Chie to check the board.",
        );
        assert_eq!(
            accept_reply(raw, &reply, &[]),
            Ok((raw.to_string(), Vec::new())),
            "reverted"
        );
        let filtered = filter(raw, &reply);
        assert_eq!(
            accept(raw, &filtered.transcript, &[]),
            Ok(raw.to_string()),
            "judged as accept judges it"
        );
    }

    #[test]
    fn accept_reply_exempts_a_spelled_term_from_the_budget() {
        let raw = "I want A-I-J-U to check the board.";
        let transcript = "I want Aiju to check the new deployment plan board.";
        let reply = Reply {
            replacements: vec![],
            learned: strings(&["Aiju"]),
            transcript: transcript.to_string(),
        };
        assert_eq!(
            accept(raw, transcript, &[]),
            Err(Reject::NewWords),
            "over budget alone"
        );
        assert_eq!(
            accept_reply(raw, &reply, &[]),
            Ok((transcript.to_string(), strings(&["Aiju"]))),
            "the spelled term is exempt"
        );
    }

    #[test]
    fn a_dotted_clock_time_fixed_to_a_colon_is_accepted() {
        assert_eq!(
            accept(
                "The call is at 9.43 a.m. tomorrow.",
                "The call is at 9:43 a.m. tomorrow.",
                &[]
            ),
            Ok("The call is at 9:43 a.m. tomorrow.".to_string()),
            "one time"
        );
    }

    #[test]
    fn four_rewritten_times_in_a_short_transcript_exceed_the_new_word_budget() {
        assert_eq!(
            accept(
                "Meet at 9.15 a.m. then 10.30 a.m. then 11.45 a.m. and 2.20 p.m. today.",
                "Meet at 9:15 a.m. then 10:30 a.m. then 11:45 a.m. and 2:20 p.m. today.",
                &[]
            ),
            Err(Reject::NewWords),
            "four times"
        );
    }

    #[test]
    fn the_request_lists_the_terms_then_the_transcript() {
        assert_eq!(
            request("a b", &terms(&["X", "Y"])),
            "Glossary:\nX\nY\n\nTranscript:\na b",
            "terms"
        );
        assert_eq!(
            request("a b", &[]),
            "Glossary: none\n\nTranscript:\na b",
            "no terms"
        );
    }

    #[test]
    fn every_label_is_pinned() {
        assert_eq!(
            [
                Reject::Empty,
                Reject::Fenced,
                Reject::NoWords,
                Reject::TooShort,
                Reject::NewWords,
                Reject::CutOff,
            ]
            .map(Reject::label),
            [
                "rejected-empty",
                "rejected-fenced",
                "rejected-no-words",
                "rejected-too-short",
                "rejected-new-words",
                "rejected-cut-off",
            ],
            "labels"
        );
    }

    #[test]
    fn faithful_replies_are_kept() {
        let claude = terms(&["Claude"]);
        assert_eq!(
            accept(CLAWED, "Ask Claude to check the board.", &claude),
            Ok("Ask Claude to check the board.".to_string()),
            "a glossary fix"
        );
        assert_eq!(
            accept(
                RAW30,
                "Tell the team the deploy moves to Wednesday and the release notes go out Thursday.",
                &[]
            ),
            Ok("Tell the team the deploy moves to Wednesday and the release notes go out Thursday.".to_string()),
            "a self-correction dropped"
        );
        assert_eq!(
            accept(RAW30, RAW30, &[]),
            Ok(RAW30.to_string()),
            "unchanged"
        );
        assert_eq!(
            accept("x y", "  x y  ", &[]),
            Ok("x y".to_string()),
            "trimmed"
        );
        assert_eq!(
            accept(
                "I mean, I mean, what they really meant was, but what I actually meant was ship it.",
                "Ship it.",
                &[]
            ),
            Ok("Ship it.".to_string()),
            "the user's example, under the ratio floor"
        );
        assert_eq!(
            accept(
                "Ask the agent.",
                "Ask the Agent GTD.",
                &terms(&["Agent GTD"])
            ),
            Ok("Ask the Agent GTD.".to_string()),
            "a multi-word term"
        );
        assert_eq!(
            accept("Yes.", "Yes", &[]),
            Ok("Yes".to_string()),
            "one word"
        );
    }

    #[test]
    fn leaked_trailing_json_braces_and_quotes_are_stripped() {
        assert_eq!(
            accept(
                "Before we get started, though, what questions do you have for me?",
                "Before we get started, though, what questions do you have for me?\" }",
                &[]
            ),
            Ok("Before we get started, though, what questions do you have for me?".to_string()),
            "a leaked quote, space and brace after the transcript"
        );
        assert_eq!(
            accept("Ship it.", "Ship it.\"}", &[]),
            Ok("Ship it.".to_string()),
            "a leaked quote then brace"
        );
        assert_eq!(
            accept("Ship it.", "Ship it.}\"", &[]),
            Ok("Ship it.".to_string()),
            "a leaked brace then quote"
        );
        assert_eq!(
            accept("Ship it.", "Ship it.}\u{201d}", &[]),
            Ok("Ship it.".to_string()),
            "a leaked brace then right double quotation mark"
        );
        assert_eq!(
            accept("He said \"yes\"", "He said \"yes\"}", &[]),
            Ok("He said \"yes\"".to_string()),
            "raw keeps its own quote"
        );
        assert_eq!(
            accept(
                "He said \u{201c}yes\u{201d}",
                "He said \u{201c}yes\u{201d}}",
                &[]
            ),
            Ok("He said \u{201c}yes\u{201d}".to_string()),
            "raw keeps its own right double quotation mark"
        );
        assert_eq!(
            accept("Write {x}", "Write {x}\"", &[]),
            Ok("Write {x}".to_string()),
            "raw keeps its own brace"
        );
        assert_eq!(
            accept("He said \"yes\" ", "He said \"yes\" }", &[]),
            Ok("He said \"yes\"".to_string()),
            "raw's trailing space is not part of its ending"
        );
        assert_eq!(
            accept("Write {x} ", "Write {x}\"", &[]),
            Ok("Write {x}".to_string()),
            "raw's trailing space keeps its own brace"
        );
        assert_eq!(
            accept("He said \"yes\"", "He said \"yes\"\"}", &[]),
            Ok("He said \"yes\"\"".to_string()),
            "a leak equal to raw's last character is kept, the documented limit"
        );
        assert_eq!(
            accept("Write {x}", "Write {x}}\"", &[]),
            Ok("Write {x}}".to_string()),
            "only the run outside raw's own tail is stripped"
        );
        assert_eq!(
            accept_reply("Ship it.", &Reply::new("Ship it.\"}"), &[]),
            Ok(("Ship it.".to_string(), Vec::new())),
            "the same stripping through accept_reply"
        );
        assert_eq!(
            accept("Ship it.", "\"}", &[]),
            Err(Reject::Empty),
            "a reply that is only a leak is empty"
        );
    }

    #[test]
    fn empty_fenced_and_wordless_replies_are_refused() {
        assert_eq!(accept(CLAWED, "", &[]), Err(Reject::Empty), "empty");
        assert_eq!(accept(CLAWED, "   ", &[]), Err(Reject::Empty), "blank");
        assert_eq!(
            accept(CLAWED, "```\nAsk Clawed to check the board.\n```", &[]),
            Err(Reject::Fenced),
            "fenced"
        );
        assert_eq!(
            accept("Ship it.", "\u{2014}", &[]),
            Err(Reject::NoWords),
            "no words in the reply"
        );
        assert_eq!(
            accept("...", "...", &[]),
            Err(Reject::NoWords),
            "no words in the transcript"
        );
    }

    #[test]
    fn a_long_transcript_must_keep_a_third_of_its_words() {
        assert_eq!(
            accept(
                RAW30,
                "Tell the team the deploy moves to Wednesday Thursday.",
                &[]
            ),
            Err(Reject::TooShort),
            "9 of 30"
        );
        assert_eq!(
            accept(
                RAW30,
                "Tell the team the deploy moves to Wednesday and Thursday.",
                &[]
            ),
            Ok("Tell the team the deploy moves to Wednesday and Thursday.".to_string()),
            "10 of 30"
        );
        assert_eq!(
            accept(TWENTY, "one two three four five twenty.", &[]),
            Err(Reject::TooShort),
            "6 of 20"
        );
        let nineteen = TWENTY.replace(" nineteen", "");
        assert_eq!(
            accept(&nineteen, "one two three twenty.", &[]),
            Ok("one two three twenty.".to_string()),
            "no ratio below 20 words"
        );
    }

    #[test]
    fn words_beyond_the_budget_are_refused() {
        // 11 kept tokens, a run of 6, so 5 new.
        assert_eq!(
            accept(
                CLAWED,
                "Here is the corrected transcript: Ask Clawed to check the board.",
                &terms(&["Claude"])
            ),
            Err(Reject::NewWords),
            "a preamble"
        );
        assert_eq!(
            accept(CLAWED, "a b c d Ask", &[]),
            Err(Reject::NewWords),
            "over budget before cut off"
        );
    }

    #[test]
    fn small_changes_within_budget_are_kept() {
        assert_eq!(
            accept(
                "We are gonna ship it today.",
                "We are going ship it today.",
                &[]
            ),
            Ok("We are going ship it today.".to_string()),
            "a changed word, within budget"
        );
        assert_eq!(
            accept("Ship it today.", "Ship today it.", &[]),
            Ok("Ship today it.".to_string()),
            "reordered, within budget"
        );
    }

    #[test]
    fn the_short_budget_is_three() {
        let raw = "Tomorrow I want that we should ship the build.";
        for reply in [
            "Tomorrow I want us to ship the build.",
            "Tomorrow I want us all to ship the build.",
        ] {
            assert_eq!(accept(raw, reply, &[]), Ok(reply.to_string()), "{reply}");
        }
        assert_eq!(
            accept(raw, "Tomorrow I want us all to go ship the build.", &[]),
            Err(Reject::NewWords),
            "4 new"
        );
    }

    fn raw(n: usize) -> String {
        format!(
            "{}.",
            (1..=n)
                .map(|i| format!("w{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    fn extra(k: usize) -> String {
        (1..=k)
            .map(|i| format!("n{i}"))
            .collect::<Vec<_>>()
            .iter()
            .fold(String::new(), |acc, w| acc + w + " ")
    }

    #[test]
    fn the_budget_scales_with_length() {
        let reply = format!("{}{}", extra(12), raw(240));
        assert_eq!(accept(&raw(240), &reply, &[]), Ok(reply.clone()), "12");
        assert_eq!(
            accept(&raw(240), &format!("{}{}", extra(13), raw(240)), &[]),
            Err(Reject::NewWords),
            "13"
        );
        let even = format!(
            "{}.",
            (2..=240)
                .step_by(2)
                .map(|i| format!("w{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        assert_eq!(
            accept(&raw(240), &even, &[]),
            Ok(even.clone()),
            "subsequence"
        );
        assert_eq!(
            accept(&raw(79), &format!("{}{}", extra(4), raw(79)), &[]),
            Err(Reject::NewWords),
            "79 words"
        );
        let ok = format!("{}{}", extra(4), raw(80));
        assert_eq!(accept(&raw(80), &ok, &[]), Ok(ok.clone()), "80 words");
        assert_eq!(
            accept(&raw(80), &format!("{}{}", extra(5), raw(80)), &[]),
            Err(Reject::NewWords),
            "80 words, 5"
        );
    }

    #[test]
    fn glossary_words_never_count() {
        let t = terms(&["Claude", "Agent GTD"]);
        let ok = "Please now ask Claude and Agent GTD to check the board.";
        assert_eq!(accept(CLAWED, ok, &t), Ok(ok.to_string()), "3 new");
        assert_eq!(
            accept(
                CLAWED,
                "Please now ask Claude and Agent GTD to go check the board.",
                &t
            ),
            Err(Reject::NewWords),
            "4 new"
        );
    }

    #[test]
    fn the_new_word_count_is_minimal_and_ordered() {
        let ok = "Ask x Clawed y to z check the board.";
        assert_eq!(accept(CLAWED, ok, &[]), Ok(ok.to_string()), "3 new");
        assert_eq!(
            accept(CLAWED, "Ask x Clawed y to z check w the board.", &[]),
            Err(Reject::NewWords),
            "4 new"
        );
        assert_eq!(
            accept("a b c d e f x.", "x a b c d e f x.", &[]),
            Ok("x a b c d e f x.".to_string()),
            "repeat"
        );
        assert_eq!(
            accept("a b c d e f.", "e d c b a f.", &[]),
            Err(Reject::NewWords),
            "reversed"
        );
        assert_eq!(
            accept("a b c d e f.", "d c b a e f.", &[]),
            Ok("d c b a e f.".to_string()),
            "run of three"
        );
    }

    #[test]
    fn a_repair_with_unrelated_tails_is_cut_off() {
        assert_eq!(
            accept(
                "Ship it Friday, I mean, no wait, scrap that.",
                "Ship it Monday.",
                &[]
            ),
            Err(Reject::CutOff),
            "within budget but the tails share nothing"
        );
    }

    #[test]
    fn a_reply_that_stops_early_is_refused() {
        assert_eq!(
            accept(
                RAW30,
                "Tell the team I mean, I mean, what they really meant was, but what I actually meant was the deploy",
                &[]
            ),
            Err(Reject::CutOff),
            "cut off"
        );
    }
}
