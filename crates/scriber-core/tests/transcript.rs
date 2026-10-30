//! The silence filter, the hallucination filter and the joining rule.

#[cfg(test)]
mod tests {
    use scriber_core::ports::Phrase;
    use scriber_core::transcript::{
        HALLUCINATIONS, SAMPLES_PER_CENTISECOND, SEGMENT_SEPARATOR, SHORT_SEGMENT, SILENCE_RMS,
        SILENCE_WINDOW, TRAILING_PADDING, TRAILING_SILENCE_RMS, is_silent, join, keep, kept_text,
        phantom, phrase_audio, samples_at, trim_trailing_silence,
    };

    #[test]
    fn the_constants_are_pinned() {
        assert_eq!(SILENCE_WINDOW, 1_600, "100 ms at 16 kHz");
        assert!((SILENCE_RMS - 0.01).abs() < f32::EPSILON, "-40 dBFS");
        assert_eq!(TRAILING_PADDING, 8_000, "500 ms at 16 kHz");
        assert!(
            (TRAILING_SILENCE_RMS - 0.002).abs() < f32::EPSILON,
            "-54 dBFS"
        );
        assert_eq!(SHORT_SEGMENT, 160_000, "10 s at 16 kHz");
        assert_eq!(SEGMENT_SEPARATOR, "\n\n", "a blank line between segments");
    }

    #[test]
    fn nothing_and_digital_silence_are_silent() {
        assert!(is_silent(&[]), "no samples");
        assert!(is_silent(&vec![0.0; 16_000]), "zeros");
    }

    #[test]
    fn a_quiet_room_is_silent() {
        let hiss: Vec<f32> = (0..16_000)
            .map(|i| if i % 2 == 0 { 0.005 } else { -0.005 })
            .collect();
        assert!(is_silent(&hiss), "-46 dBFS of noise");
    }

    #[test]
    fn speech_is_not_silent() {
        let speech: Vec<f32> = (0..16_000)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect();
        assert!(!is_silent(&speech), "a loud signal of both signs");
    }

    #[test]
    fn one_loud_window_is_enough_however_long_the_quiet() {
        // Ten seconds of zeros with one loud sample: the whole segment's RMS
        // is far below the threshold, but its window's is above it.
        let mut samples = vec![0.0; 160_000];
        samples[80_000] = 0.5;
        assert!(!is_silent(&samples), "the loud window counts");
    }

    #[test]
    fn the_threshold_itself_is_not_silent() {
        assert!(!is_silent(&[SILENCE_RMS]), "exactly at the level");
        assert!(is_silent(&[SILENCE_RMS * 0.99]), "just below it");
    }

    #[test]
    fn text_is_trimmed_and_empty_text_is_dropped() {
        assert_eq!(
            keep(16_000, "  Hello there.\n"),
            Some("Hello there.".to_string()),
            "trimmed"
        );
        assert_eq!(keep(16_000, ""), None, "empty");
        assert_eq!(keep(16_000, " \n "), None, "blank");
    }

    #[test]
    fn a_short_segment_that_is_only_a_hallucination_is_dropped() {
        for h in HALLUCINATIONS {
            assert_eq!(keep(SHORT_SEGMENT - 1, &format!(" {h} ")), None, "{h}");
        }
        assert_eq!(keep(16_000, "THANK YOU."), None, "case is ignored");
        assert_eq!(keep(16_000, "[blank_audio]"), None, "case is ignored");
    }

    #[test]
    fn the_hallucination_list_is_pinned() {
        assert_eq!(
            HALLUCINATIONS,
            [
                "Thank you.",
                "Thank you",
                "Thank you very much.",
                "Thanks for watching!",
                "Thanks for watching.",
                "Thank you for watching.",
                "Thank you for watching!",
                "Bye.",
                "you",
                "[BLANK_AUDIO]",
                "(silence)",
                "Subtitles by the Amara.org community",
            ],
            "the list"
        );
    }

    #[test]
    fn a_long_segment_keeps_the_same_words() {
        assert_eq!(
            keep(SHORT_SEGMENT, "Thank you."),
            Some("Thank you.".to_string()),
            "at the limit"
        );
    }

    #[test]
    fn a_hallucination_inside_real_text_is_kept() {
        assert_eq!(
            keep(16_000, "Thank you. Now the next item."),
            Some("Thank you. Now the next item.".to_string()),
            "only a whole-text match is dropped"
        );
    }

    #[test]
    fn text_without_a_letter_or_digit_is_dropped_at_any_length() {
        for n in [16_000, SHORT_SEGMENT] {
            for t in [".", "...", " . ", "?!", "-", "\u{2014}"] {
                assert_eq!(keep(n, t), None, "{t:?} at {n} samples");
            }
        }
    }

    #[test]
    fn text_with_a_letter_or_digit_is_kept_trimmed() {
        for n in [16_000, SHORT_SEGMENT] {
            for (heard, kept) in [
                ("1.", "1."),
                ("\u{e9}.", "\u{e9}."),
                ("OK.", "OK."),
                (" OK. ", "OK."),
            ] {
                assert_eq!(
                    keep(n, heard),
                    Some(kept.to_string()),
                    "{heard:?} at {n} samples"
                );
            }
        }
    }

    #[test]
    fn segments_join_as_paragraphs_in_order() {
        assert_eq!(
            join(["one", "two", "three"]),
            "one\n\ntwo\n\nthree",
            "three"
        );
        assert_eq!(join(["only"]), "only", "one");
        assert_eq!(join([]), "", "none");
    }

    fn loud(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect()
    }

    fn trimmed(parts: &[Vec<f32>]) -> usize {
        trim_trailing_silence(&parts.concat()).len()
    }

    #[test]
    fn trimming_keeps_padding_after_the_last_loud_window() {
        assert_eq!(trimmed(&[loud(32_000)]), 32_000, "a");
        assert_eq!(trimmed(&[loud(16_000), vec![0.0; 48_000]]), 24_000, "b");
        assert_eq!(trimmed(&[loud(16_000), vec![0.0; 8_000]]), 24_000, "c1");
        assert_eq!(trimmed(&[loud(16_000), vec![0.0; 7_999]]), 23_999, "c2");
    }

    #[test]
    fn a_window_exactly_at_the_level_is_loud() {
        assert_eq!(
            trim_trailing_silence(&[TRAILING_SILENCE_RMS]).len(),
            1,
            "d1"
        );
        assert_eq!(
            trim_trailing_silence(&[TRAILING_SILENCE_RMS * 0.99]).len(),
            0,
            "d2"
        );
        assert_eq!(
            trimmed(&[vec![0.0; 1_600], vec![TRAILING_SILENCE_RMS]]),
            1_601,
            "d3"
        );
        assert_eq!(
            trimmed(&[
                loud(1_600),
                vec![0.0; 16_000],
                vec![TRAILING_SILENCE_RMS * 0.99]
            ]),
            9_600,
            "d4"
        );
    }

    #[test]
    fn faint_clicks_and_leading_silence_are_handled() {
        let click = vec![0.005; 1_600];
        assert_eq!(
            trimmed(&[loud(16_000), vec![0.0; 16_000], click, vec![0.0; 16_000]]),
            41_600,
            "e: the click reaches the trim level, so it is kept"
        );
        assert_eq!(trimmed(&[vec![0.0; 16_000], loud(100)]), 16_100, "f");
        assert_eq!(
            trimmed(&[vec![0.0; 16_000], loud(1_600), vec![0.0; 32_000]]),
            25_600,
            "g"
        );
    }

    #[test]
    fn quiet_speech_between_the_levels_is_kept() {
        assert_eq!(
            trimmed(&[loud(16_000), vec![0.005; 16_000], vec![0.0; 48_000]]),
            40_000,
            "speech at 0.005 is under the silence level but over the trim level"
        );
    }

    #[test]
    fn quiet_audio_trims_to_nothing() {
        assert_eq!(trimmed(&[]), 0, "h1");
        assert_eq!(trimmed(&[vec![0.0; 16_000]]), 0, "h2");
    }

    #[test]
    fn the_trimmed_audio_is_a_prefix_of_the_input() {
        let s = [loud(16_000), vec![0.0; 48_000]].concat();
        let r = trim_trailing_silence(&s);
        assert_eq!(r, &s[..24_000], "prefix");
        assert!(std::ptr::eq(r.as_ptr(), s.as_ptr()), "same start");
    }

    #[test]
    fn centiseconds_convert_to_samples() {
        assert_eq!(SAMPLES_PER_CENTISECOND, 160, "16 kHz");
        assert_eq!(samples_at(0), 0, "zero");
        assert_eq!(samples_at(1), 160, "one");
        assert_eq!(samples_at(2_980), 476_800, "29.8 s");
        assert_eq!(samples_at(-1), 0, "negative");
        assert_eq!(samples_at(i64::MIN), 0, "most negative");
        assert_eq!(samples_at(i64::MAX), usize::MAX, "saturates");
    }

    fn p(text: &str, start: usize, end: usize) -> Phrase {
        Phrase {
            text: text.into(),
            start,
            end,
        }
    }

    /// Ten seconds of speech, then two of digital silence.
    fn audio_a() -> Vec<f32> {
        [loud(320_000), vec![0.0; 32_000]].concat()
    }

    fn hello() -> Phrase {
        p(" Hello there.", 0, 320_000)
    }

    #[test]
    fn a_phantom_last_phrase_over_silence_is_dropped() {
        let a = audio_a();
        let thanks = p(" Thank you.", 320_000, 352_000);
        assert_eq!(
            kept_text(&a, &[hello(), thanks.clone()]),
            Some("Hello there.".to_string()),
            "a"
        );
        let hum = [loud(320_000), vec![SILENCE_RMS * 0.99; 32_000]].concat();
        assert_eq!(
            kept_text(&hum, &[hello(), thanks.clone()]),
            Some("Hello there.".to_string()),
            "a2"
        );
        let spoken = [loud(320_000), loud(32_000)].concat();
        assert_eq!(
            kept_text(&spoken, &[hello(), thanks]),
            Some("Hello there. Thank you.".to_string()),
            "b"
        );
    }

    #[test]
    fn a_window_at_the_silence_level_is_evidence_of_speech() {
        let short = p(" Thank you.", 320_000, 350_401);
        let mut at = audio_a();
        if let Some(s) = at.get_mut(350_400) {
            *s = SILENCE_RMS;
        }
        assert_eq!(
            kept_text(&at, &[hello(), short.clone()]),
            Some("Hello there. Thank you.".to_string()),
            "b2 at the level"
        );
        let mut below = audio_a();
        if let Some(s) = below.get_mut(350_400) {
            *s = SILENCE_RMS * 0.99;
        }
        assert_eq!(
            kept_text(&below, &[hello(), short]),
            Some("Hello there.".to_string()),
            "b2 below the level"
        );
    }

    #[test]
    fn only_a_whole_text_match_of_the_last_phrase_is_dropped() {
        let a = audio_a();
        assert_eq!(
            kept_text(&a, &[hello(), p(" Thank you. Okay.", 320_000, 352_000)]),
            Some("Hello there. Thank you. Okay.".to_string()),
            "c"
        );
        assert_eq!(
            kept_text(&a, &[hello(), p("  thank YOU.  ", 320_000, 352_000)]),
            Some("Hello there.".to_string()),
            "d"
        );
        let first = [vec![0.0; 32_000], loud(320_000)].concat();
        assert_eq!(
            kept_text(
                &first,
                &[
                    p(" Thank you.", 0, 32_000),
                    p(" Hello there.", 32_000, 352_000)
                ]
            ),
            Some("Thank you. Hello there.".to_string()),
            "e"
        );
        assert_eq!(
            kept_text(
                &a,
                &[
                    hello(),
                    p(" Thank you.", 320_000, 336_000),
                    p(" Thank you.", 336_000, 352_000)
                ]
            ),
            Some("Hello there. Thank you.".to_string()),
            "f"
        );
    }

    #[test]
    fn the_last_phrase_is_judged_on_its_own_span() {
        let a = audio_a();
        assert_eq!(
            kept_text(&a, &[hello(), p(" Thank you.", 318_400, 352_000)]),
            Some("Hello there. Thank you.".to_string()),
            "g: start is honoured"
        );
        for (start, end) in [
            (320_000, 352_000),
            (320_000, 400_000),
            (400_000, 410_000),
            (345_000, 330_000),
        ] {
            assert_eq!(
                kept_text(&a, &[hello(), p(" Thank you.", start, end)]),
                Some("Hello there.".to_string()),
                "g: {start}..{end}"
            );
        }
    }

    #[test]
    fn a_single_phrase_follows_the_same_rules() {
        assert_eq!(
            kept_text(&vec![0.0; 16_000], &[p(" Thank you.", 0, 16_000)]),
            None,
            "h silent"
        );
        assert_eq!(
            kept_text(&loud(176_000), &[p(" Thank you.", 0, 176_000)]),
            Some("Thank you.".to_string()),
            "h spoken, long"
        );
        assert_eq!(
            kept_text(&loud(16_000), &[p(" Thank you.", 0, 16_000)]),
            None,
            "h spoken, short"
        );
        assert_eq!(kept_text(&loud(16_000), &[]), None, "i");
        assert_eq!(kept_text(&loud(16_000), &[p(" .", 0, 16_000)]), None, "j");
    }

    #[test]
    fn a_phrase_audio_is_its_span_clamped_to_the_audio() {
        let a = audio_a();
        assert_eq!(phrase_audio(&a, &p("x", 400_000, 410_000)).len(), 0, "past");
        assert_eq!(
            phrase_audio(&a, &p("x", 345_000, 330_000)).len(),
            0,
            "inverted"
        );
        assert_eq!(
            phrase_audio(&a, &p("x", 320_000, 400_000)).len(),
            32_000,
            "clamped"
        );
        let inside = phrase_audio(&a, &p("x", 10, 20));
        assert_eq!(inside.len(), 10, "inside");
        assert!(
            std::ptr::eq(inside.as_ptr(), a[10..].as_ptr()),
            "same start"
        );
    }

    #[test]
    fn a_phantom_is_an_invention_over_silence() {
        let a = audio_a();
        assert!(phantom(&a, &p(" Thank you.", 320_000, 352_000)), "invented");
        assert!(!phantom(&a, &p(" Hello.", 320_000, 352_000)), "real words");
    }
}
