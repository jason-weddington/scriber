//! Resampling to 16 kHz mono, and the WAV files kept for Retry.

#[cfg(test)]
mod tests {
    use scriber_core::audio::{
        SAMPLE_RATE, WavError, decode_wav, duration_ms, encode_wav, to_mono_16k,
    };
    use scriber_core::ports::Captured;

    fn captured(samples: Vec<f32>, channels: u16, sample_rate: u32) -> Captured {
        Captured {
            samples,
            channels,
            sample_rate,
        }
    }

    fn close(got: &[f32], want: &[f32]) {
        assert_eq!(got.len(), want.len(), "length: got {got:?}, want {want:?}");
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert!((g - w).abs() < 1e-5, "sample {i}: got {g}, want {w}");
        }
    }

    #[test]
    fn the_rate_is_16_khz() {
        assert_eq!(SAMPLE_RATE, 16_000, "Whisper's input rate");
    }

    #[test]
    fn mono_16k_passes_through_unchanged() {
        let samples = vec![0.0, 0.25, -0.5, 1.0];
        assert_eq!(
            to_mono_16k(&captured(samples.clone(), 1, 16_000)),
            samples,
            "nothing to do"
        );
    }

    #[test]
    fn channels_are_averaged_per_frame() {
        let got = to_mono_16k(&captured(vec![0.3, 0.6, 0.9, -0.3, 0.0, 0.0], 3, 16_000));
        close(&got, &[0.6, -0.1]);
    }

    #[test]
    fn a_trailing_partial_frame_is_dropped() {
        let got = to_mono_16k(&captured(vec![0.2, 0.4, 0.6, 0.8, 0.9], 2, 16_000));
        close(&got, &[0.3, 0.7]);
    }

    #[test]
    fn stereo_48k_is_averaged_and_takes_every_third_frame() {
        // Frame f: left f/100, right f/100 + 0.02, so mono is f/100 + 0.01.
        let samples: Vec<f32> = (0..9u8)
            .flat_map(|f| {
                let left = f32::from(f) / 100.0;
                [left, left + 0.02]
            })
            .collect();
        let got = to_mono_16k(&captured(samples, 2, 48_000));
        close(&got, &[0.01, 0.04, 0.07]);
    }

    #[test]
    fn upsampling_interpolates_between_neighbours() {
        let got = to_mono_16k(&captured(vec![0.0, 1.0, 0.5], 1, 8_000));
        // The last frame has no right neighbour, so it is held.
        close(&got, &[0.0, 0.5, 1.0, 0.75, 0.5, 0.5]);
    }

    #[test]
    fn a_non_integer_ratio_interpolates_at_the_exact_position() {
        let ramp: Vec<f32> = (0..44_100u32)
            .map(|i| f32::from(u16::try_from(i % 1000).unwrap()))
            .collect();
        let got = to_mono_16k(&captured(ramp, 1, 44_100));
        assert_eq!(got.len(), 16_000, "one second in, one second out");
        // Output 1 sits at input 2.75625; output 2 at 5.5125.
        close(&got[..3], &[0.0, 2.75625, 5.5125]);
    }

    #[test]
    fn no_channels_or_no_rate_gives_nothing() {
        assert!(
            to_mono_16k(&captured(vec![0.5; 4], 0, 16_000)).is_empty(),
            "zero channels"
        );
        assert!(
            to_mono_16k(&captured(vec![0.5; 4], 1, 0)).is_empty(),
            "zero rate"
        );
        assert!(
            to_mono_16k(&captured(vec![], 1, 48_000)).is_empty(),
            "no samples"
        );
    }

    #[test]
    fn a_wav_file_is_the_canonical_44_byte_header_then_pcm() {
        let wav = encode_wav(&[0.0, 1.0, -1.0, 0.5, 2.0, -2.0]);
        let mut want = Vec::new();
        want.extend_from_slice(b"RIFF");
        want.extend_from_slice(&48u32.to_le_bytes());
        want.extend_from_slice(b"WAVEfmt ");
        want.extend_from_slice(&[16, 0, 0, 0, 1, 0, 1, 0]);
        want.extend_from_slice(&16_000u32.to_le_bytes());
        want.extend_from_slice(&32_000u32.to_le_bytes());
        want.extend_from_slice(&[2, 0, 16, 0]);
        want.extend_from_slice(b"data");
        want.extend_from_slice(&12u32.to_le_bytes());
        for v in [0i16, 32767, -32767, 16384, 32767, -32767] {
            want.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(wav, want, "header and samples, byte for byte");
    }

    #[test]
    fn a_wav_file_reads_back_within_one_step() {
        let samples = vec![0.0, 0.123, -0.456, 0.999, -1.0];
        let back = decode_wav(&encode_wav(&samples)).unwrap();
        assert_eq!(back.len(), samples.len(), "every sample");
        for (b, s) in back.iter().zip(&samples) {
            assert!((b - s).abs() < 1.0 / 16_000.0, "{b} vs {s}");
        }
    }

    /// A WAV file built chunk by chunk.
    fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = b"WAVE".to_vec();
        for (id, data) in chunks {
            body.extend_from_slice(*id);
            body.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
            body.extend_from_slice(data);
            if data.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn fmt(format: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&format.to_le_bytes());
        f.extend_from_slice(&channels.to_le_bytes());
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&rate.saturating_mul(2).to_le_bytes());
        f.extend_from_slice(&2u16.to_le_bytes());
        f.extend_from_slice(&bits.to_le_bytes());
        f
    }

    fn pcm(values: &[i16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn other_chunks_are_skipped_including_odd_sized_ones() {
        // ffmpeg writes a LIST chunk; an odd size is followed by a pad byte.
        let wav = riff(&[
            (b"fmt ", fmt(1, 1, 16_000, 16)),
            (b"LIST", b"INFOISFTx".to_vec()),
            (b"data", pcm(&[-32768, 16384, 0, 32767])),
        ]);
        let got = decode_wav(&wav).unwrap();
        assert_eq!(
            got,
            vec![-1.0, 0.5, 0.0, 32767.0 / 32768.0],
            "divided by 32768"
        );
    }

    #[test]
    fn the_data_chunk_may_come_before_fmt_and_a_stray_odd_byte_is_ignored() {
        let mut data = pcm(&[16384]);
        data.push(7);
        let wav = riff(&[(b"data", data), (b"fmt ", fmt(1, 1, 16_000, 16))]);
        assert_eq!(decode_wav(&wav).unwrap(), vec![0.5], "one whole sample");
    }

    #[test]
    fn anything_but_riff_wave_is_not_a_wav_file() {
        let good = encode_wav(&[0.5]);
        let mut not_riff = good.clone();
        not_riff[0] = b'X';
        let mut not_wave = good.clone();
        not_wave[8] = b'X';
        for (name, bytes) in [
            ("riff", not_riff),
            ("wave", not_wave),
            ("short", b"RIFF".to_vec()),
        ] {
            assert_eq!(decode_wav(&bytes), Err(WavError::NotWav), "{name}");
        }
    }

    #[test]
    fn a_chunk_past_the_end_is_truncated() {
        let mut wav = encode_wav(&[0.5, 0.5]);
        let _: Option<u8> = wav.pop();
        assert_eq!(
            decode_wav(&wav),
            Err(WavError::Truncated),
            "data runs short"
        );
        let mut header = riff(&[(b"fmt ", fmt(1, 1, 16_000, 16))]);
        header.extend_from_slice(b"dat");
        assert_eq!(
            decode_wav(&header),
            Err(WavError::Truncated),
            "a partial chunk header"
        );
        let mut no_size = riff(&[(b"fmt ", fmt(1, 1, 16_000, 16))]);
        no_size.extend_from_slice(b"data\x01\x00");
        assert_eq!(
            decode_wav(&no_size),
            Err(WavError::Truncated),
            "a partial chunk size"
        );
    }

    #[test]
    fn both_chunks_are_required() {
        assert_eq!(
            decode_wav(&riff(&[(b"data", pcm(&[1]))])),
            Err(WavError::MissingChunk("fmt")),
            "no fmt"
        );
        assert_eq!(
            decode_wav(&riff(&[(b"fmt ", fmt(1, 1, 16_000, 16))])),
            Err(WavError::MissingChunk("data")),
            "no data"
        );
    }

    #[test]
    fn only_16_khz_mono_16_bit_pcm_is_read() {
        for (name, f) in [
            ("float", fmt(3, 1, 16_000, 16)),
            ("stereo", fmt(1, 2, 16_000, 16)),
            ("44.1 kHz", fmt(1, 1, 44_100, 16)),
            ("24-bit", fmt(1, 1, 16_000, 24)),
            ("short fmt", vec![1, 0]),
        ] {
            let wav = riff(&[(b"fmt ", f), (b"data", pcm(&[1]))]);
            assert_eq!(decode_wav(&wav), Err(WavError::Unsupported), "{name}");
        }
    }

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(WavError::NotWav.to_string(), "not a WAV file", "not wav");
        assert_eq!(
            WavError::Truncated.to_string(),
            "the WAV file is truncated",
            "truncated"
        );
        assert_eq!(
            WavError::MissingChunk("data").to_string(),
            "the WAV file has no data chunk",
            "missing"
        );
        assert_eq!(
            WavError::Unsupported.to_string(),
            "the WAV file is not 16 kHz mono 16-bit PCM",
            "unsupported"
        );
    }

    #[test]
    fn durations_are_whole_milliseconds_at_16_khz() {
        assert_eq!(duration_ms(0), 0, "none");
        assert_eq!(duration_ms(15), 0, "under a millisecond");
        assert_eq!(duration_ms(16), 1, "one");
        assert_eq!(duration_ms(24_000), 1_500, "a second and a half");
    }
}
