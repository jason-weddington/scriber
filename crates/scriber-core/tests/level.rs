//! The input level behind the window's waveform: its pinned constants and
//! how sample blocks become readings.

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use scriber_core::level::{CEILING_DBFS, FLOOR_DBFS, Meter, Tap, WINDOW_MS, feeder, level};
    use scriber_core::ports::Captured;

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
        let mut tap = Tap::new(2, 16_000);
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
        let tap = Arc::new(Mutex::new(Tap::new(1, 16_000)));
        let reports = Arc::new(Mutex::new(Vec::new()));
        let (seen, watched) = (Arc::clone(&reports), Arc::clone(&tap));
        let mut deliver = feeder(Arc::clone(&tap), move |level| {
            assert!(watched.try_lock().is_ok(), "the tap is unlocked");
            seen.lock().unwrap().push(level);
        });
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
}
