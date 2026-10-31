//! The input level behind the window's waveform: its pinned constants and
//! how sample blocks become readings.

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use scriber_core::level::{
        CEILING_DBFS, FLOOR_DBFS, MIN_SPLIT_SEGMENT_MS, Meter, SilenceProfile, Splitter, Tap,
        WINDOW_MS, feeder, is_silent_window, level, silence_profile,
    };
    use scriber_core::ports::Captured;
    use scriber_core::transcript::TRAILING_SILENCE_RMS;

    const LOUD: f32 = 0.5;
    const SILENT: f32 = 0.0;

    /// `parts` of (value, count) runs, concatenated.
    fn runs(parts: &[(f32, usize)]) -> Vec<f32> {
        parts
            .iter()
            .flat_map(|(value, n)| std::iter::repeat_n(*value, *n))
            .collect()
    }

    /// A tap at rate 20 mono (one sample per window) splitting at 1.5 s.
    fn slow_tap() -> Tap {
        Tap::new(1, 20, 1_500)
    }

    /// Feeds `samples` one at a time; answers the 1-based sample counts
    /// after which a segment closed.
    fn cuts(tap: &mut Tap, samples: &[f32]) -> Vec<usize> {
        let mut at = Vec::new();
        for (i, sample) in samples.iter().enumerate() {
            let before = tap.waiting();
            let _level = tap.feed(&[*sample]);
            if tap.waiting() > before {
                at.push(i.saturating_add(1));
            }
        }
        at
    }

    fn close(got: f64, want: f64, what: &str) {
        assert!((got - want).abs() < 1e-3, "{what}: {got} is not {want}");
    }

    /// A sine at `peak` amplitude, `n` samples at 16 kHz (440 Hz).
    fn sine(peak: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = f32::from(u16::try_from(i % 16_000).unwrap()) / 16_000.0;
                peak * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
            })
            .collect()
    }

    #[test]
    fn the_pinned_constants() {
        assert_eq!(
            (WINDOW_MS, FLOOR_DBFS, CEILING_DBFS),
            (50, -52.0, -6.0),
            "window, floor, ceiling"
        );
    }

    #[test]
    fn levels_are_decibels_mapped_from_the_floor_to_the_ceiling() {
        assert_eq!(level(0.0), 0.0, "silence");
        assert_eq!(level(f64::NAN), 0.0, "not a number reads as silence");
        assert_eq!(level(0.001), 0.0, "-60 dBFS clamps to 0");
        assert_eq!(level(0.002), 0.0, "about -54 dBFS clamps to 0");
        assert_eq!(level(1.0), 1.0, "0 dBFS");
        assert_eq!(level(2.0), 1.0, "above full scale");
        assert_eq!(level(0.6), 1.0, "about -4.4 dBFS clamps to 1");
        close(level(10f64.powf(-14.0 / 20.0)), 38.0 / 46.0, "-14 dBFS");
        close(
            level(10f64.powf(-52.0 / 20.0)),
            0.0,
            "the floor, room silence",
        );
        close(level(10f64.powf(-6.0 / 20.0)), 1.0, "the ceiling");
        close(
            level(10f64.powf(-20.0 / 20.0)),
            32.0 / 46.0,
            "the old ceiling, now below the top",
        );
        close(level(10f64.powf(-29.0 / 20.0)), 0.5, "half way");
        close(level(0.01), 12.0 / 46.0, "-40 dBFS");
        close(level(10f64.powf(-41.0 / 20.0)), 11.0 / 46.0, "quiet speech");
        close(level(10f64.powf(-33.0 / 20.0)), 19.0 / 46.0, "loud speech");
        close(
            level(10f64.powf(-27.0 / 20.0)),
            25.0 / 46.0,
            "the loudest measured",
        );
        for amplitude in [0.005, 0.01, 0.02] {
            let l = level(amplitude);
            assert!(l > 0.05 && l < 0.95, "{amplitude} is not clamped: {l}");
        }
    }

    #[test]
    fn a_window_of_silence_reads_zero_and_a_full_scale_sine_one() {
        let mut meter = Meter::new(1, 16_000);
        assert_eq!(meter.feed(&[0.0; 800]), Some(0.0), "silence");
        // A full-scale sine's RMS is 1/sqrt(2): -3 dBFS, above the ceiling.
        assert_eq!(meter.feed(&sine(1.0, 800)), Some(1.0), "full-scale sine");
        let quiet = 0.01_f32 * std::f32::consts::SQRT_2;
        close(
            meter.feed(&sine(quiet, 800)).unwrap(),
            12.0 / 46.0,
            "a -40 dBFS sine",
        );
    }

    #[test]
    fn one_reading_per_window_of_every_channel_with_leftovers_carried_over() {
        // 48 kHz stereo: 50 ms is 2,400 frames, 4,800 samples.
        let mut meter = Meter::new(2, 48_000);
        assert_eq!(meter.feed(&vec![0.02; 4_799]), None, "one short");
        let reading = meter.feed(&[0.02; 2]).unwrap();
        close(reading, level(0.02), "the window's RMS");
        assert_eq!(
            meter.feed(&vec![0.01; 4_798]),
            None,
            "the leftover sample counts"
        );
        close(meter.feed(&[0.01]).unwrap(), 12.0 / 46.0, "a fresh window");
    }

    #[test]
    fn a_block_that_completes_several_windows_reports_the_loudest() {
        let mut loud_first = Meter::new(1, 16_000);
        let mut block = vec![0.02; 800];
        block.extend(vec![0.005; 800]);
        close(loud_first.feed(&block).unwrap(), level(0.02), "loud, quiet");
        let mut quiet_first = Meter::new(1, 16_000);
        block.reverse();
        close(
            quiet_first.feed(&block).unwrap(),
            level(0.02),
            "quiet, loud",
        );
    }

    #[test]
    fn a_format_with_no_window_never_reads() {
        assert_eq!(
            Meter::new(0, 48_000).feed(&vec![1.0; 10_000]),
            None,
            "no channels"
        );
        assert_eq!(Meter::new(1, 0).feed(&vec![1.0; 10_000]), None, "no rate");
        assert_eq!(
            Meter::new(1, 19).feed(&vec![1.0; 10_000]),
            None,
            "under one frame"
        );
        assert!(Meter::new(1, 20).feed(&[1.0]).is_some(), "one frame");
    }

    #[test]
    fn a_tap_keeps_every_block_and_meters_it() {
        let mut tap = Tap::new(2, 16_000, 0);
        assert_eq!(tap.feed(&vec![0.01; 1_000]), None, "not a window yet");
        close(tap.feed(&[0.01; 600]).unwrap(), level(0.01), "a window");
        assert_eq!(
            tap.take(),
            Captured::new(vec![0.01; 1_600], 2, 16_000),
            "everything, with its format"
        );
        assert_eq!(tap.take(), Captured::new(vec![], 2, 16_000), "empty after");
    }

    #[test]
    fn the_feeder_keeps_every_block_and_reports_outside_the_lock() {
        let tap = Arc::new(Mutex::new(Tap::new(1, 16_000, 0)));
        let reports = Arc::new(Mutex::new(Vec::new()));
        let (seen, watched) = (Arc::clone(&reports), Arc::clone(&tap));
        let mut deliver = feeder(
            Arc::clone(&tap),
            move |level| {
                assert!(watched.try_lock().is_ok(), "the tap is unlocked");
                seen.lock().unwrap().push(level);
            },
            || panic!("no split"),
        );
        deliver(&vec![0.02; 700], &());
        deliver(&vec![0.02; 100], &());
        deliver(&[0.02; 10], &());
        assert_eq!(reports.lock().unwrap().len(), 1, "one window, one report");
        close(reports.lock().unwrap()[0], level(0.02), "its level");
        assert_eq!(
            tap.lock().unwrap().take().samples.len(),
            810,
            "every sample kept"
        );
    }

    #[test]
    fn the_split_constants() {
        assert_eq!(
            MIN_SPLIT_SEGMENT_MS, 2_000,
            "the shortest segment a split closes"
        );
        assert!(is_silent_window(0.002_f64.powi(2) * 0.99, 1), "below");
        assert!(!is_silent_window(0.0021_f64.powi(2), 1), "above");
        assert!(is_silent_window(0.0, 0), "an empty window of nothing");
        assert!(
            is_silent_window(0.002_f64.powi(2) * 800.0 * 0.99, 800),
            "a 50 ms window just below"
        );
        assert!(
            !is_silent_window(0.002_f64.powi(2) * 800.0 * 1.01, 800),
            "a 50 ms window just above"
        );
    }

    #[test]
    fn a_sustained_silence_after_speech_closes_the_segment() {
        let mut tap = slow_tap();
        let input = runs(&[(LOUD, 10), (SILENT, 30)]);
        assert_eq!(cuts(&mut tap, &input), vec![40], "after sample 40");
        assert_eq!(
            tap.take_closed(),
            vec![Captured::new(input, 1, 20)],
            "the first 40 samples"
        );
        assert!(tap.take_closed().is_empty(), "taken once");
    }

    #[test]
    fn the_threshold_itself_is_silence() {
        for (quiet, splits) in [
            (TRAILING_SILENCE_RMS, true),
            (-TRAILING_SILENCE_RMS, true),
            (f32::from_bits(TRAILING_SILENCE_RMS.to_bits() + 1), false),
        ] {
            let mut tap = slow_tap();
            let got = cuts(&mut tap, &runs(&[(LOUD, 10), (quiet, 30)]));
            assert_eq!(!got.is_empty(), splits, "{quiet}");
        }
    }

    #[test]
    fn one_window_short_does_not_split() {
        let mut tap = slow_tap();
        let input = runs(&[(LOUD, 10), (SILENT, 29)]);
        assert!(cuts(&mut tap, &input).is_empty(), "no split");
        assert_eq!(tap.take(), Captured::new(input, 1, 20), "all 39");
    }

    #[test]
    fn speech_one_window_before_the_deadline_restarts_the_run() {
        let mut tap = slow_tap();
        let input = runs(&[(LOUD, 10), (SILENT, 29), (LOUD, 1), (SILENT, 29)]);
        assert!(cuts(&mut tap, &input).is_empty(), "no split");
        assert_eq!(cuts(&mut tap, &[SILENT]), vec![1], "one more splits");
        assert_eq!(tap.take_closed()[0].samples.len(), 70, "after sample 70");
    }

    #[test]
    fn a_segment_under_two_seconds_is_not_cut() {
        let mut tap = slow_tap();
        let input = runs(&[(LOUD, 9), (SILENT, 31)]);
        assert_eq!(cuts(&mut tap, &input), vec![40], "not at 39, at 40");
    }

    #[test]
    fn all_silence_never_splits() {
        let mut tap = slow_tap();
        assert!(cuts(&mut tap, &[SILENT; 200]).is_empty(), "no split");
    }

    #[test]
    fn a_split_of_zero_is_off() {
        let mut tap = Tap::new(1, 20, 0);
        assert!(
            cuts(&mut tap, &runs(&[(LOUD, 10), (SILENT, 100)])).is_empty(),
            "off"
        );
    }

    #[test]
    fn the_next_segment_needs_its_own_speech_and_run() {
        let mut tap = slow_tap();
        let first = runs(&[(LOUD, 10), (SILENT, 30)]);
        assert_eq!(cuts(&mut tap, &first), vec![40], "the first split");
        let next = runs(&[(SILENT, 30), (LOUD, 10), (SILENT, 30)]);
        assert!(cuts(&mut tap, &next[..30]).is_empty(), "silence alone");
        assert_eq!(cuts(&mut tap, &next[30..]), vec![40], "speech then silence");
        assert_eq!(
            tap.take_closed(),
            vec![Captured::new(first, 1, 20), Captured::new(next, 1, 20)],
            "exactly those 70"
        );
    }

    #[test]
    fn a_split_at_a_real_format_loses_and_duplicates_nothing() {
        let rate = 48_000;
        let second = 96_000; // stereo samples per second
        let mut input: Vec<f32> = Vec::new();
        let voice = |n: usize, into: &mut Vec<f32>| {
            into.extend((0..n).map(|i| 0.25 + f32::from(u16::try_from(i % 1_000).unwrap()) * 1e-4));
        };
        voice(second, &mut input);
        input.extend(vec![0.0; second * 3 / 2]);
        voice(second, &mut input);
        input.extend(vec![0.0; second * 3 / 2]);
        voice(second / 2, &mut input);
        assert_eq!(input.len(), 528_000, "the input");
        let mut tap = Tap::new(2, rate, 1_500);
        for block in input.chunks(997) {
            let _level = tap.feed(block);
        }
        let closed = tap.take_closed();
        assert_eq!(
            closed
                .iter()
                .map(|c| (c.samples.len(), c.channels, c.sample_rate))
                .collect::<Vec<_>>(),
            vec![(240_000, 2, rate), (240_000, 2, rate)],
            "two segments"
        );
        let tail = tap.take();
        assert_eq!(tail.samples.len(), 48_000, "the tail");
        let joined: Vec<f32> = closed
            .into_iter()
            .flat_map(|c| c.samples)
            .chain(tail.samples)
            .collect();
        assert_eq!(joined, input, "sample-exact");
    }

    #[test]
    fn take_merges_closed_segments_waiting() {
        let mut tap = slow_tap();
        let input = runs(&[(LOUD, 10), (SILENT, 30), (LOUD, 5)]);
        assert_eq!(cuts(&mut tap, &input), vec![40], "one closed");
        assert_eq!(tap.waiting(), 1, "waiting");
        assert_eq!(tap.take(), Captured::new(input, 1, 20), "all, in order");
        assert!(tap.take_closed().is_empty(), "nothing left");
        assert_eq!(tap.take(), Captured::new(vec![], 1, 20), "empty after");
        // The splitter starts afresh: a new segment needs its own speech.
        assert!(cuts(&mut tap, &[SILENT; 40]).is_empty(), "no stale state");
    }

    #[test]
    fn a_take_mid_window_resets_the_splitter() {
        let mut tap = slow_tap();
        assert!(
            cuts(&mut tap, &runs(&[(LOUD, 10), (SILENT, 29)])).is_empty(),
            "one short"
        );
        let _all = tap.take();
        assert!(
            cuts(&mut tap, &[SILENT; 30]).is_empty(),
            "nothing heard since"
        );
        let mut stereo = Tap::new(2, 40, 1_500); // window of 4 samples
        assert!(cuts(&mut stereo, &[LOUD; 3]).is_empty(), "a partial window");
        let _partial = stereo.take();
        // Were the 3 loud samples kept, the first window would be loud.
        assert!(cuts(&mut stereo, &[SILENT; 400]).is_empty(), "fresh window");
    }

    #[test]
    fn the_feeder_signals_a_split_once_outside_the_lock() {
        let tap = Arc::new(Mutex::new(slow_tap()));
        let count = Arc::new(Mutex::new(0_u32));
        let (counted, watched) = (Arc::clone(&count), Arc::clone(&tap));
        let mut deliver = feeder(
            Arc::clone(&tap),
            |_| {},
            move || {
                assert!(watched.try_lock().is_ok(), "the tap is unlocked");
                *counted.lock().unwrap() += 1;
            },
        );
        deliver(&[LOUD; 10], &());
        assert_eq!(*count.lock().unwrap(), 0, "speech");
        deliver(&[SILENT; 29], &());
        assert_eq!(*count.lock().unwrap(), 0, "one short");
        deliver(&[SILENT; 1], &());
        assert_eq!(*count.lock().unwrap(), 1, "the split");
        deliver(&[SILENT; 5], &());
        assert_eq!(*count.lock().unwrap(), 1, "no more");
        let closed = tap.lock().unwrap().take_closed();
        assert_eq!(closed.len(), 1, "one segment");
        assert_eq!(closed[0].samples.len(), 40, "of 40 samples");
    }

    #[test]
    fn a_format_with_no_window_never_cuts() {
        for mut splitter in [Splitter::new(0, 48_000, 1_500), Splitter::new(1, 19, 1_500)] {
            let input = runs(&[(LOUD, 1_000), (SILENT, 1_000)]);
            assert!(input.iter().all(|s| !splitter.push(*s)), "{splitter:?}");
        }
    }

    #[test]
    fn the_needed_run_rounds_up_to_whole_windows() {
        // 1,501 ms needs 31 windows at rate 20 mono.
        let mut splitter = Splitter::new(1, 20, 1_501);
        let input = runs(&[(LOUD, 10), (SILENT, 30)]);
        assert!(input.iter().all(|s| !splitter.push(*s)), "30 is short");
        assert!(splitter.push(SILENT), "31 cuts");
        // Stereo at rate 40: a window is 2 frames, 4 samples; 2 s is 160.
        let mut stereo = Splitter::new(2, 40, 1_500);
        let stereo_input = runs(&[(LOUD, 40), (SILENT, 119)]);
        assert!(stereo_input.iter().all(|s| !stereo.push(*s)), "one short");
        assert!(stereo.push(SILENT), "at 160 samples");
    }

    #[test]
    fn the_silence_profile_of_a_capture() {
        assert_eq!(
            silence_profile(&runs(&[(LOUD, 10), (SILENT, 30)]), 1, 20),
            SilenceProfile {
                longest_silence_ms: 1_500,
                quietest_rms: 0.0
            },
            "speech then silence"
        );
        assert_eq!(
            silence_profile(&[LOUD; 40], 1, 20),
            SilenceProfile {
                longest_silence_ms: 0,
                quietest_rms: 0.5
            },
            "speech"
        );
        let none = SilenceProfile {
            longest_silence_ms: 0,
            quietest_rms: 0.0,
        };
        assert_eq!(silence_profile(&[], 1, 20), none, "empty");
        assert_eq!(silence_profile(&[LOUD; 40], 0, 20), none, "no channels");
        assert_eq!(
            silence_profile(
                &runs(&[(SILENT, 3), (LOUD, 1), (SILENT, 2), (LOUD, 1)]),
                2,
                20
            ),
            SilenceProfile {
                longest_silence_ms: 50,
                quietest_rms: 0.0
            },
            "stereo windows of 2; the partial window is ignored"
        );
        assert_eq!(
            silence_profile(&[0.123_456, 0.123_456], 1, 20).quietest_rms,
            0.1235,
            "rounded to 4 places"
        );
        assert_eq!(
            silence_profile(&runs(&[(SILENT, 2), (LOUD, 1), (SILENT, 3)]), 1, 20)
                .longest_silence_ms,
            150,
            "the longest run, not the first"
        );
    }
}
