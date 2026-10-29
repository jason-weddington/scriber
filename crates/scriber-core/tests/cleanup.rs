//! The cleanup pass's request and the sanity check on the model's reply.

#[cfg(test)]
mod tests {
    use scriber_core::cleanup::{
        CLEANED, FAILED, MAX_TERMS, MIN_WORDS_FOR_RATIO, NEW_WORDS_MIN, NEW_WORDS_PER, OFF, PROMPT,
        Reject, UNCHANGED, accept, request,
    };

    const RAW30: &str = "Tell the team I mean, I mean, what they really meant was, but what I actually meant was the deploy moves to Wednesday and the release notes go out Thursday.";

    const CLAWED: &str = "Ask Clawed to check the board.";

    const TWENTY: &str = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty.";

    fn terms(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| (*t).to_string()).collect()
    }

    #[test]
    fn the_constants_are_pinned() {
        assert_eq!(MAX_TERMS, 200, "terms");
        assert_eq!(MIN_WORDS_FOR_RATIO, 20, "ratio floor");
        assert_eq!(NEW_WORDS_MIN, 3, "new words min");
        assert_eq!(NEW_WORDS_PER, 20, "new words per");
        assert_eq!(
            [OFF, CLEANED, UNCHANGED, FAILED],
            ["off", "cleaned", "unchanged", "failed"],
            "labels"
        );
        assert_eq!(
            PROMPT,
            "You correct a dictated transcript. Make only two kinds of change. First, where a word or phrase was misheard and should be one of the glossary terms, replace it with that term spelled exactly as in the glossary. Second, where the speaker corrected themselves while speaking (a false start, a repeated phrase, or words such as \"I mean\" or \"what I actually meant was\"), work out what they were trying to say. If their final restatement says it cleanly, keep that restatement exactly as spoken. If it does not, write what they were trying to say in words that match the rest of the transcript, its vocabulary and its tone, adding as few words they did not say as you can. Either way, delete the abandoned words. Change nothing else: keep every other word, the word order, the spelling, the punctuation and the paragraph breaks exactly as they are. Do not rephrase, summarize, add, explain or answer anything else in the transcript. Reply with the corrected transcript only, with no preamble, quotes or notes. If nothing needs changing, reply with the transcript unchanged.",
            "prompt"
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
