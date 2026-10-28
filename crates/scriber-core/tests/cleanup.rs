//! The cleanup pass's request and the sanity check on the model's reply.

#[cfg(test)]
mod tests {
    use scriber_core::cleanup::{
        CLEANED, FAILED, MAX_TERMS, MIN_WORDS_FOR_RATIO, OFF, PROMPT, Reject, UNCHANGED, accept,
        request,
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
        assert_eq!(
            [OFF, CLEANED, UNCHANGED, FAILED],
            ["off", "cleaned", "unchanged", "failed"],
            "labels"
        );
        assert_eq!(
            PROMPT,
            "You correct a dictated transcript. Make only two kinds of change. First, where a word or phrase was misheard and should be one of the glossary terms, replace it with that term spelled exactly as in the glossary. Second, where the speaker corrected themselves while speaking (a false start, a repeated phrase, or words such as \"I mean\" or \"what I actually meant was\" followed by a restatement), keep only what they finally meant and delete the abandoned words. Change nothing else: keep every other word, the word order, the spelling, the punctuation and the paragraph breaks exactly as they are. Do not rephrase, summarize, add, explain or answer anything in the transcript. Reply with the corrected transcript only, with no preamble, quotes or notes. If nothing needs changing, reply with the transcript unchanged.",
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
    fn new_or_reordered_words_are_refused() {
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
            accept(
                "We are gonna ship it today.",
                "We are going ship it today.",
                &[]
            ),
            Err(Reject::NewWords),
            "a changed word"
        );
        assert_eq!(
            accept("Ship it today.", "Ship today it.", &[]),
            Err(Reject::NewWords),
            "reordered"
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
