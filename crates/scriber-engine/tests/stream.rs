//! The stream recorder: pushed 16 kHz mono samples through the core's tap.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use scriber_engine::{
        InputDevices, Recorder as _, STREAM_CHANNELS, STREAM_DEFAULT_LABEL, STREAM_SAMPLE_RATE,
        StreamRecorder,
    };

    fn sine_f32(samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| {
                let i = f32::from(u16::try_from(i).unwrap());
                0.5 * (2.0 * std::f32::consts::PI * 440.0 * i / 16_000.0).sin()
            })
            .collect()
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::as_conversions,
        reason = "a test sine in -0.5..0.5 scaled to i16 fits"
    )]
    fn to_pcm16(x: f32) -> i16 {
        (x * 32767.0).round() as i16
    }

    fn devices(label: &str) -> InputDevices {
        InputDevices {
            default_device: Some(label.to_string()),
            others: vec![],
        }
    }

    #[test]
    fn the_format_is_pinned() {
        assert_eq!(
            (STREAM_SAMPLE_RATE, STREAM_CHANNELS, STREAM_DEFAULT_LABEL),
            (16_000, 1, "Remote microphone")
        );
    }

    #[test]
    fn pcm16_is_captured_as_f32_over_32768() {
        let r = StreamRecorder::new();
        let _name1 = r.start("", 0).unwrap();
        let pcm: Vec<i16> = sine_f32(32_000).iter().map(|x| to_pcm16(*x)).collect();
        for chunk in pcm.chunks(4_000) {
            r.push_pcm16(chunk);
        }
        let got = r.stop().unwrap();
        let want: Vec<f32> = pcm.iter().map(|s| f32::from(*s) / 32768.0).collect();
        assert_eq!(got.samples, want);
        assert_eq!(got.channels, STREAM_CHANNELS);
        assert_eq!(got.sample_rate, STREAM_SAMPLE_RATE);
        assert_eq!(r.dropped_samples(), 0);
        assert_eq!(r.pushed_samples(), 32_000);
    }

    #[test]
    fn f32_is_captured_unconverted() {
        let r = StreamRecorder::new();
        let _name2 = r.start("", 0).unwrap();
        let s = sine_f32(4_000);
        r.push(&s);
        assert_eq!(r.stop().unwrap().samples, s);
    }

    #[test]
    fn silence_closes_a_segment_and_notifies_once() {
        let r = StreamRecorder::new();
        let count = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&count);
        r.on_split(Arc::new(move || {
            let _before = c.fetch_add(1, Ordering::SeqCst);
        }));
        let _name3 = r.start("", 1500).unwrap();
        let mut pushed = sine_f32(40_000);
        pushed.extend(vec![0.0; 25_600]);
        for chunk in pushed.chunks(4_000) {
            r.push(chunk);
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let closed = r.take_closed();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].samples, pushed[..64_000]);
        assert_eq!(r.take_closed(), Vec::new());
        assert_eq!(r.stop().unwrap().samples, pushed[64_000..]);
    }

    #[test]
    fn pushes_before_start_and_after_stop_are_dropped() {
        let r = StreamRecorder::new();
        r.push(&[0.5_f32; 4_000]);
        assert_eq!(r.stop().unwrap_err().to_string(), "not recording");
        assert_eq!(r.take_closed(), Vec::new());
        let _name4 = r.start("", 0).unwrap();
        assert_eq!(r.dropped_samples(), 4_000, "start does not change it");
        r.push(&[]);
        assert_eq!((r.dropped_samples(), r.pushed_samples()), (4_000, 4_000));
        let _captured1 = r.stop().unwrap();
        assert_eq!(r.stop().unwrap_err().to_string(), "not recording");
        r.push(&[0.5_f32; 1_600]);
        assert_eq!(r.dropped_samples(), 5_600);
        assert_eq!(r.pushed_samples(), 5_600);
    }

    #[test]
    fn the_label_is_the_device_or_the_default() {
        for r in [StreamRecorder::default(), StreamRecorder::new()] {
            assert_eq!(r.inputs().unwrap(), devices("Remote microphone"));
            assert_eq!(r.start("Pixel 9 mic", 0).unwrap(), "Pixel 9 mic");
            assert_eq!(r.inputs().unwrap(), devices("Pixel 9 mic"));
            let _captured2 = r.stop().unwrap();
            assert_eq!(r.inputs().unwrap(), devices("Pixel 9 mic"));
            assert_eq!(r.start("", 0).unwrap(), "Remote microphone");
            assert_eq!(r.inputs().unwrap(), devices("Remote microphone"));
            let _captured3 = r.stop().unwrap();
            assert_eq!(r.inputs().unwrap(), devices("Remote microphone"));
        }
    }

    #[test]
    fn a_clone_pushes_into_the_same_capture() {
        let r = StreamRecorder::new();
        let _name5 = r.start("", 0).unwrap();
        r.clone().push(&[0.25; 10]);
        assert_eq!(r.stop().unwrap().samples, vec![0.25; 10]);
    }

    #[test]
    fn levels_are_one_reading_per_push_at_most() {
        let r = StreamRecorder::new();
        let _name6 = r.start("", 0).unwrap();
        r.push(&sine_f32(4_000));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&calls);
        r.with_level(Arc::new(move |l| sink.lock().unwrap().push(l)));
        let _name7 = r.start("", 0).unwrap();
        r.push(&sine_f32(4_000));
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert!(calls.lock().unwrap()[0] > 0.0);
        r.push(&[0.0; 4_000]);
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert_eq!(calls.lock().unwrap()[1], 0.0);
        let _name8 = r.start("", 0).unwrap();
        calls.lock().unwrap().clear();
        r.push(&sine_f32(400));
        assert_eq!(calls.lock().unwrap().len(), 0);
        r.push(&sine_f32(400));
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_push_with_no_sink_or_notifier_is_fine() {
        let r = StreamRecorder::new();
        let _name9 = r.start("", 1500).unwrap();
        r.push(&sine_f32(4_000));
        r.push(&vec![0.0; 40_000]);
        assert_eq!(r.take_closed().len(), 1);
    }

    #[test]
    fn start_over_start_drops_the_untaken_samples() {
        let r = StreamRecorder::new();
        let _name10 = r.start("", 0).unwrap();
        r.push(&[0.5_f32; 4_000]);
        let _name11 = r.start("", 0).unwrap();
        r.push(&[0.5_f32; 1_600]);
        assert_eq!(r.stop().unwrap().samples.len(), 1_600);
        assert_eq!(r.dropped_samples(), 4_000);
    }

    #[test]
    fn a_notifier_set_after_start_still_fires() {
        let r = StreamRecorder::new();
        let _name12 = r.start("", 1500).unwrap();
        r.push(&sine_f32(40_000));
        let count = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&count);
        r.on_split(Arc::new(move || {
            let _before = c.fetch_add(1, Ordering::SeqCst);
        }));
        r.push(&vec![0.0; 25_600]);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn every_pushed_sample_is_kept_or_dropped() {
        let r = StreamRecorder::new();
        let mut returned = 0;
        let _name13 = r.start("", 1500).unwrap();
        let mut a = sine_f32(40_000);
        a.extend(vec![0.0; 25_600]);
        r.push(&a);
        returned += r
            .take_closed()
            .iter()
            .map(|c| c.samples.len())
            .sum::<usize>();
        let _name14 = r.start("", 1500).unwrap();
        r.push(&sine_f32(5_000));
        returned += r.stop().unwrap().samples.len();
        r.push(&[0.5; 700]);
        assert_eq!(
            r.pushed_samples(),
            r.dropped_samples() + u64::try_from(returned).unwrap()
        );
    }
}
