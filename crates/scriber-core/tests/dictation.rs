//! The dictation session's state machine, against fake ports.

mod common;

#[cfg(test)]
mod tests {
    use scriber_core::audio::{decode_wav, encode_wav};
    use scriber_core::dictation::{
        COPIED, Dictation, DictationError, Job, NOTHING_HEARD, input_fallback, model_missing,
    };
    use scriber_core::model::{Action, Phase, Snapshot};
    use scriber_core::ports::{Captured, Phrase, PortError};
    use scriber_core::vocabulary::STYLE;

    use scriber_core::library::{entry_path, list};
    use scriber_core::ports::Files as _;
    use scriber_core::store::{LEARNED, LEARNING_LOG, MICROPHONE_LOG, STARTED, TRANSCRIPT};

    use scriber_core::cleanup::PROMPT;
    use scriber_core::ports::Ports;
    use scriber_core::settings::SETTINGS;
    use serde_json::{Value, json};

    use crate::common::{World, at as time, captured_speech, speech};

    fn idle(w: &World) -> Dictation {
        Dictation::recover(&w.ports())
    }

    fn snap(d: &Dictation, w: &World) -> Snapshot {
        d.snapshot(&w.ports())
    }

    /// Transcribes `job` with the world's transcriber and hands the result back.
    fn finish(d: &mut Dictation, w: &World, job: &Job) {
        let snapshot = d.complete(w.run(job), &w.ports());
        assert_eq!(snapshot, snap(d, w), "complete answers with the snapshot");
    }

    /// A session driven to `phase`. Returns the jobs it made that are still
    /// pending (only `Transcribing` leaves one).
    fn at(w: &World, phase: Phase) -> (Dictation, Vec<Job>) {
        let p = w.ports();
        let mut d = idle(w);
        if phase == Phase::Idle {
            return (d, vec![]);
        }
        d.new_session(&p).unwrap();
        if phase == Phase::Ready {
            return (d, vec![]);
        }
        d.record(&p).unwrap();
        if phase == Phase::Recording {
            return (d, vec![]);
        }
        let job = d.pause(&p).unwrap().unwrap();
        if phase == Phase::Paused {
            return (d, vec![job]);
        }
        if phase == Phase::Failed {
            w.transcriber
                .answers
                .borrow_mut()
                .push_back(Err("model exploded".to_string()));
        }
        assert!(d.done(&p).unwrap().is_none(), "nothing was recording");
        if phase == Phase::Transcribing {
            return (d, vec![job]);
        }
        finish(&mut d, w, &job);
        assert_eq!(d.snapshot(&p).phase, phase, "reached");
        (d, vec![])
    }

    const PHASES: [Phase; 7] = [
        Phase::Idle,
        Phase::Ready,
        Phase::Recording,
        Phase::Paused,
        Phase::Transcribing,
        Phase::Finished,
        Phase::Failed,
    ];

    const ACTIONS: [Action; 10] = [
        Action::New,
        Action::Record,
        Action::Pause,
        Action::Resume,
        Action::Done,
        Action::Discard,
        Action::Retry,
        Action::Copy,
        Action::Accept,
        Action::UndoLearn,
    ];

    fn act(d: &mut Dictation, w: &World, action: Action) -> Result<(), DictationError> {
        d.apply(action, "edited", &w.ports()).map(drop)
    }

    /// The legal actions per phase (docs/design.md, "States").
    fn legal(phase: Phase, action: Action) -> bool {
        use Action as A;
        match phase {
            Phase::Idle => matches!(action, A::New),
            Phase::Ready => matches!(action, A::Record | A::Discard),
            Phase::Recording => matches!(action, A::Pause | A::Done | A::Discard),
            Phase::Paused => matches!(action, A::Resume | A::Done | A::Discard),
            Phase::Transcribing => false,
            Phase::Finished => matches!(
                action,
                A::New | A::Discard | A::Copy | A::Accept | A::UndoLearn
            ),
            Phase::Failed => matches!(action, A::New | A::Discard | A::Retry),
        }
    }

    #[test]
    fn every_illegal_action_is_refused_and_changes_nothing() {
        for phase in PHASES {
            for action in ACTIONS {
                let w = World::default();
                let (mut d, _jobs) = at(&w, phase);
                let before = snap(&d, &w);
                let result = act(&mut d, &w, action);
                if legal(phase, action) {
                    assert!(
                        result.is_ok(),
                        "{action:?} from {phase:?} is legal: {result:?}"
                    );
                } else {
                    assert_eq!(
                        result,
                        Err(DictationError::Illegal { action, phase }),
                        "{action:?} from {phase:?}"
                    );
                    assert_eq!(
                        snap(&d, &w),
                        before,
                        "{action:?} from {phase:?} changed the session"
                    );
                }
            }
        }
    }

    #[test]
    fn refusals_read_as_sentences() {
        let names: Vec<String> = ACTIONS.iter().map(ToString::to_string).collect();
        assert_eq!(
            names,
            [
                "start a new session",
                "record",
                "pause",
                "resume",
                "finish",
                "discard",
                "retry",
                "copy",
                "accept",
                "undo learning"
            ],
            "action names"
        );
        let phases: Vec<String> = PHASES.iter().map(ToString::to_string).collect();
        assert_eq!(
            phases,
            [
                "idle",
                "ready",
                "recording",
                "paused",
                "transcribing",
                "finished",
                "failed"
            ],
            "phase names"
        );
        let e = DictationError::Illegal {
            action: Action::Pause,
            phase: Phase::Ready,
        };
        assert_eq!(e.to_string(), "Cannot pause while ready.", "sentence");
    }

    #[test]
    fn a_fresh_app_is_idle_and_empty() {
        let w = World::default();
        assert_eq!(
            snap(&idle(&w), &w),
            Snapshot {
                revision: 0,
                phase: Phase::Idle,
                recorded_ms: 0,
                segments: 0,
                pending: 0,
                text: String::new(),
                error: None,
                notice: None,
                model_error: None,
                download: None,
                library_revision: 0,
                microphone: None,
                learned: vec![],
                partial: String::new(),
                raw_text: None,
            },
            "idle"
        );
    }

    #[test]
    fn the_whole_flow_copies_the_transcript_in_paragraphs() {
        let w = World::default();
        let p = w.ports();
        let mut d = idle(&w);
        d.new_session(&p).unwrap();
        assert_eq!(snap(&d, &w).phase, Phase::Ready, "new");
        d.record(&p).unwrap();
        let first = d.pause(&p).unwrap().unwrap();
        finish(&mut d, &w, &first);
        assert_eq!(
            snap(&d, &w).phase,
            Phase::Paused,
            "a completed segment does not finish a paused session"
        );
        d.resume(&p).unwrap();
        let second = d.done(&p).unwrap().unwrap();
        assert_eq!(
            snap(&d, &w).phase,
            Phase::Transcribing,
            "waits for the last segment"
        );
        assert!(w.clipboard.writes.borrow().is_empty(), "nothing copied yet");
        finish(&mut d, &w, &second);
        let s = snap(&d, &w);
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.text, "segment 0\n\nsegment 1", "trimmed, joined in order");
        assert_eq!(s.recorded_ms, 2_000, "two seconds of audio");
        assert_eq!((s.segments, s.pending), (2, 0), "counts");
        assert_eq!(s.notice.as_deref(), Some(COPIED), "copied");
        assert_eq!(
            *w.clipboard.writes.borrow(),
            vec![s.text.clone()],
            "the clipboard holds it"
        );
        assert_eq!(
            *w.recorder.calls.borrow(),
            ["start", "stop", "start", "stop"],
            "recorder calls"
        );
    }

    #[test]
    fn the_recorded_time_is_the_length_of_the_audio_kept() {
        let w = World::default();
        {
            let mut captures = w.recorder.captures.borrow_mut();
            captures.push_back(Ok(Captured {
                samples: speech(24_000),
                channels: 1,
                sample_rate: 16_000,
            }));
            captures.push_back(Err("unplugged".to_string()));
            captures.push_back(Ok(Captured {
                samples: speech(48_000),
                channels: 2,
                sample_rate: 48_000,
            }));
        }
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Ready);
        d.record(&p).unwrap();
        assert_eq!(snap(&d, &w).recorded_ms, 0, "nothing closed yet");
        let _first = d.pause(&p).unwrap();
        assert_eq!(snap(&d, &w).recorded_ms, 1_500, "1.5 s at 16 kHz");
        d.resume(&p).unwrap();
        let _lost = d.pause(&p).unwrap();
        assert_eq!(snap(&d, &w).recorded_ms, 1_500, "a lost part adds nothing");
        d.resume(&p).unwrap();
        let _third = d.pause(&p).unwrap();
        assert_eq!(snap(&d, &w).recorded_ms, 2_000, "0.5 s of 48 kHz stereo");
    }

    #[test]
    fn segments_finishing_out_of_order_still_join_in_recording_order() {
        let w = World::default();
        let p = w.ports();
        let (mut d, mut jobs) = at(&w, Phase::Paused);
        d.resume(&p).unwrap();
        jobs.push(d.pause(&p).unwrap().unwrap());
        d.resume(&p).unwrap();
        jobs.push(d.done(&p).unwrap().unwrap());
        let outcomes: Vec<_> = jobs.iter().map(|j| w.run(j)).collect();
        for o in outcomes.into_iter().rev() {
            let _snapshot = d.complete(o, &p);
        }
        assert_eq!(
            snap(&d, &w).text,
            "segment 0\n\nsegment 1\n\nsegment 2",
            "recording order"
        );
    }

    #[test]
    fn pending_counts_follow_the_transcriptions() {
        let w = World::default();
        let p = w.ports();
        let (mut d, mut jobs) = at(&w, Phase::Paused);
        d.resume(&p).unwrap();
        jobs.push(d.pause(&p).unwrap().unwrap());
        let before = snap(&d, &w);
        assert_eq!((before.segments, before.pending), (2, 2), "both in flight");
        finish(&mut d, &w, &jobs[1]);
        let after = snap(&d, &w);
        assert_eq!((after.segments, after.pending), (2, 1), "one back");
    }

    #[test]
    fn done_while_paused_with_everything_transcribed_finishes_at_once() {
        let w = World::default();
        let p = w.ports();
        let (mut d, jobs) = at(&w, Phase::Paused);
        finish(&mut d, &w, &jobs[0]);
        assert!(d.done(&p).unwrap().is_none(), "no new segment");
        assert_eq!(snap(&d, &w).phase, Phase::Finished, "nothing to wait for");
        assert_eq!(
            *w.recorder.calls.borrow(),
            ["start", "stop"],
            "the paused recorder is not stopped again"
        );
    }

    #[test]
    fn a_silent_segment_is_never_sent_to_the_transcriber() {
        let w = World::default();
        w.recorder.captures.borrow_mut().push_back(Ok(Captured {
            samples: vec![0.0; 32_000],
            channels: 1,
            sample_rate: 16_000,
        }));
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Recording);
        let job = d.done(&p).unwrap().unwrap();
        finish(&mut d, &w, &job);
        assert!(w.transcriber.calls.borrow().is_empty(), "not transcribed");
        let s = snap(&d, &w);
        assert_eq!(
            (s.phase, s.text.as_str()),
            (Phase::Finished, ""),
            "finished, empty"
        );
        assert_eq!(
            s.notice.as_deref(),
            Some(NOTHING_HEARD),
            "says nothing was heard"
        );
        assert!(
            w.clipboard.writes.borrow().is_empty(),
            "an empty transcript is not copied"
        );
    }

    #[test]
    fn trailing_silence_is_trimmed_for_the_transcriber_but_not_the_saved_audio() {
        let w = World::default();
        let long = [speech(16_000), vec![0.0; 48_000]].concat();
        w.recorder
            .captures
            .borrow_mut()
            .push_back(Ok(Captured::new(long, 1, 16_000)));
        let (_d, jobs) = at(&w, Phase::Paused);
        assert_eq!(
            decode_wav(&w.files.segment(0).unwrap()).unwrap().len(),
            64_000,
            "saved whole"
        );
        let _o = w.run(&jobs[0]);
        assert_eq!(w.transcriber.calls.borrow()[0].0, 24_000, "heard trimmed");
        assert_eq!(
            decode_wav(&w.files.segment(0).unwrap()).unwrap().len(),
            64_000,
            "still saved whole"
        );
    }

    /// Ten seconds of speech and two of silence, transcribed as "Hello."
    /// up to `split` and "Thank you." from there to the end of the trimmed
    /// audio; returns the transcript and the transcribed length.
    fn hello_then_thanks(split: usize) -> (String, usize) {
        let w = World::default();
        w.recorder.captures.borrow_mut().push_back(Ok(Captured::new(
            [speech(320_000), vec![0.0; 32_000]].concat(),
            1,
            16_000,
        )));
        w.transcriber.phrases.borrow_mut().push_back(vec![
            Phrase {
                text: " Hello.".into(),
                start: 0,
                end: split,
            },
            Phrase {
                text: " Thank you.".into(),
                start: split,
                end: 328_000,
            },
        ]);
        let p = w.ports();
        let (mut d, jobs) = at(&w, Phase::Paused);
        let _none = d.done(&p).unwrap();
        finish(&mut d, &w, &jobs[0]);
        let heard = w.transcriber.calls.borrow()[0].0;
        (snap(&d, &w).text, heard)
    }

    #[test]
    fn a_phantom_last_phrase_over_silence_is_dropped() {
        let (text, heard) = hello_then_thanks(320_000);
        assert_eq!(heard, 328_000, "trimmed to the padding");
        assert_eq!(text, "Hello.", "dropped");
    }

    #[test]
    fn a_spoken_last_thank_you_is_kept() {
        let (text, heard) = hello_then_thanks(300_000);
        assert_eq!(heard, 328_000, "trimmed to the padding");
        assert_eq!(text, "Hello. Thank you.", "kept");
    }

    #[test]
    fn a_long_segment_of_mostly_room_tone_drops_its_hallucination() {
        let w = World::default();
        {
            let mut answers = w.transcriber.answers.borrow_mut();
            answers.push_back(Ok("Hello.".to_string()));
            answers.push_back(Ok(" Thank you. ".to_string()));
            answers.push_back(Ok("Goodbye.".to_string()));
            let mut captures = w.recorder.captures.borrow_mut();
            captures.push_back(Ok(captured_speech(1)));
            // 240_000 samples is >= SHORT_SEGMENT, so without the trim
            // "Thank you." would have been kept.
            captures.push_back(Ok(Captured::new(
                [speech(16_000), vec![0.0; 224_000]].concat(),
                1,
                16_000,
            )));
            captures.push_back(Ok(captured_speech(1)));
        }
        let p = w.ports();
        let (mut d, mut jobs) = at(&w, Phase::Paused);
        for _ in 0..2 {
            d.resume(&p).unwrap();
            jobs.push(d.pause(&p).unwrap().unwrap());
        }
        let _none = d.done(&p).unwrap();
        for job in &jobs {
            finish(&mut d, &w, job);
        }
        assert_eq!(snap(&d, &w).text, "Hello.\n\nGoodbye.", "dropped");
        assert_eq!(w.transcriber.calls.borrow()[1].0, 24_000, "trimmed");
    }

    #[test]
    fn retry_transcribes_the_trimmed_saved_audio() {
        let w = World::default();
        let long = [speech(16_000), vec![0.0; 48_000]].concat();
        w.recorder
            .captures
            .borrow_mut()
            .push_back(Ok(Captured::new(long, 1, 16_000)));
        let (mut d, _) = at(&w, Phase::Failed);
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Ok("Hello.".to_string()));
        let retried = d.retry(&w.ports()).unwrap();
        finish(&mut d, &w, &retried[0]);
        assert_eq!(w.transcriber.calls.borrow().len(), 2, "twice");
        assert_eq!(w.transcriber.calls.borrow()[1].0, 24_000, "trimmed");
        assert_eq!(snap(&d, &w).text, "Hello.", "kept");
    }

    #[test]
    fn a_hallucinated_segment_leaves_no_trace_in_the_transcript() {
        let w = World::default();
        {
            let mut answers = w.transcriber.answers.borrow_mut();
            answers.push_back(Ok("Hello.".to_string()));
            answers.push_back(Ok(" Thank you. ".to_string()));
            answers.push_back(Ok("Goodbye.".to_string()));
        }
        let p = w.ports();
        let (mut d, mut jobs) = at(&w, Phase::Paused);
        for _ in 0..2 {
            d.resume(&p).unwrap();
            jobs.push(d.pause(&p).unwrap().unwrap());
        }
        let _none = d.done(&p).unwrap();
        for job in &jobs {
            finish(&mut d, &w, job);
        }
        assert_eq!(
            snap(&d, &w).text,
            "Hello.\n\nGoodbye.",
            "dropped without an empty paragraph"
        );
    }

    #[test]
    fn a_punctuation_only_segment_leaves_no_paragraph() {
        let w = World::default();
        {
            let mut answers = w.transcriber.answers.borrow_mut();
            answers.push_back(Ok("Hello.".to_string()));
            answers.push_back(Ok(" . ".to_string()));
            answers.push_back(Ok("Goodbye.".to_string()));
        }
        let p = w.ports();
        let (mut d, mut jobs) = at(&w, Phase::Paused);
        for _ in 0..2 {
            d.resume(&p).unwrap();
            jobs.push(d.pause(&p).unwrap().unwrap());
        }
        let _none = d.done(&p).unwrap();
        for job in &jobs {
            finish(&mut d, &w, job);
        }
        assert_eq!(
            snap(&d, &w).text,
            "Hello.\n\nGoodbye.",
            "the punctuation-only segment leaves no paragraph"
        );
    }

    #[test]
    fn the_glossary_becomes_the_prompt_and_is_reread_for_each_session() {
        let w = World::default();
        w.files.set_vocabulary(Some("talos\nagent-gtd\n"));
        let p = w.ports();
        let (mut d, jobs) = at(&w, Phase::Paused);
        let _first = w.run(&jobs[0]);
        w.files.set_vocabulary(None);
        d.discard(&p).unwrap();
        d.record(&p).unwrap();
        let job = d.pause(&p).unwrap().unwrap();
        let _second = w.run(&job);
        assert_eq!(
            *w.transcriber.calls.borrow(),
            [
                (16_000, Some(format!("{STYLE} Glossary: talos, agent-gtd."))),
                (16_000, Some(STYLE.to_string())),
            ],
            "samples and prompt per call"
        );
    }

    #[test]
    fn the_glossary_is_read_from_the_path_in_the_settings() {
        let w = World::default();
        w.files.set_vocabulary(Some("ignored"));
        w.files
            .write("settings.json", br#"{"vocabulary":"/elsewhere/terms.txt"}"#)
            .unwrap();
        w.files.write("/elsewhere/terms.txt", b"talos\n").unwrap();
        let (_d, jobs) = at(&w, Phase::Paused);
        let _first = w.run(&jobs[0]);
        assert_eq!(
            *w.transcriber.calls.borrow(),
            [(16_000, Some(format!("{STYLE} Glossary: talos.")))],
            "the configured glossary, not vocabulary.txt"
        );
    }

    #[test]
    fn a_missing_model_is_reported_and_refuses_to_record() {
        let w = World::default();
        *w.transcriber.missing.borrow_mut() = Some("/data/models/ggml.bin".to_string());
        let p = w.ports();
        let want = "The speech model is missing from /data/models/ggml.bin.";
        assert_eq!(model_missing("/data/models/ggml.bin"), want, "sentence");
        let mut d = idle(&w);
        assert_eq!(
            snap(&d, &w).model_error.as_deref(),
            Some(want),
            "shown while idle"
        );
        d.new_session(&p).unwrap();
        let e = d.record(&p).unwrap_err();
        assert_eq!(
            e,
            DictationError::ModelMissing(want.to_string()),
            "record refused"
        );
        assert_eq!(e.to_string(), want, "as the sentence");
        assert!(
            w.recorder.calls.borrow().is_empty(),
            "the microphone never started"
        );
        assert_eq!(snap(&d, &w).phase, Phase::Ready, "still ready");
    }

    #[test]
    fn a_microphone_that_will_not_start_leaves_the_session_where_it_was() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Ready);
        *w.recorder.start_error.borrow_mut() = Some("no input device".to_string());
        assert_eq!(
            d.record(&p).unwrap_err().to_string(),
            "Could not start recording: no input device",
            "sentence"
        );
        let after = snap(&d, &w);
        assert_eq!(
            (after.phase, after.recorded_ms),
            (Phase::Ready, 0),
            "not recording"
        );
    }

    #[test]
    fn a_microphone_that_will_not_resume_leaves_the_session_paused() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Paused);
        *w.recorder.start_error.borrow_mut() = Some("no input device".to_string());
        assert_eq!(
            d.resume(&p),
            Err(DictationError::Recorder(PortError(
                "no input device".to_string()
            ))),
            "resume"
        );
        assert_eq!(snap(&d, &w).phase, Phase::Paused, "still paused");
    }

    #[test]
    fn a_microphone_that_fails_while_recording_loses_only_that_part() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Recording);
        w.recorder
            .captures
            .borrow_mut()
            .push_back(Err("device unplugged".to_string()));
        assert!(d.pause(&p).unwrap().is_none(), "no segment");
        let s = snap(&d, &w);
        assert_eq!(
            (s.phase, s.segments),
            (Phase::Paused, 0),
            "paused with nothing"
        );
        assert_eq!(
            s.notice.as_deref(),
            Some("The microphone stopped: device unplugged. The last part was not recorded."),
            "said so"
        );
        d.resume(&p).unwrap();
        assert_eq!(
            snap(&d, &w).notice,
            None,
            "the next action clears the notice"
        );
    }

    #[test]
    fn each_segment_is_kept_as_a_16k_mono_wav() {
        let w = World::default();
        // Both channels of each frame agree, so the mono mix stays loud.
        let stereo: Vec<f32> = speech(48_000).into_iter().flat_map(|s| [s, s]).collect();
        w.recorder.captures.borrow_mut().push_back(Ok(Captured {
            samples: stereo,
            channels: 2,
            sample_rate: 48_000,
        }));
        let (_d, jobs) = at(&w, Phase::Paused);
        let stored = decode_wav(&w.files.segment(0).unwrap()).unwrap();
        assert_eq!(stored.len(), 16_000, "one second at 16 kHz");
        let _outcome = w.run(&jobs[0]);
        assert_eq!(
            w.transcriber.calls.borrow()[0].0,
            16_000,
            "the job carries the same audio"
        );
    }

    #[test]
    fn a_segment_whose_audio_cannot_be_kept_still_transcribes() {
        let w = World::default();
        w.files.fail_write.set(true);
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Recording);
        let job = d.done(&p).unwrap().unwrap();
        assert_eq!(
            snap(&d, &w).notice.as_deref(),
            Some("Could not keep this part's audio for Retry: disk full."),
            "said so"
        );
        finish(&mut d, &w, &job);
        assert_eq!(snap(&d, &w).text, "segment 0", "transcribed anyway");
    }

    #[test]
    fn a_failed_segment_fails_the_session_and_retry_redoes_only_it() {
        let w = World::default();
        let p = w.ports();
        let (mut d, mut jobs) = at(&w, Phase::Paused);
        d.resume(&p).unwrap();
        jobs.push(d.done(&p).unwrap().unwrap());
        finish(&mut d, &w, &jobs[0]);
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Err("model exploded".to_string()));
        finish(&mut d, &w, &jobs[1]);
        let s = snap(&d, &w);
        assert_eq!(s.phase, Phase::Failed, "failed");
        assert_eq!(
            s.error.as_deref(),
            Some("Part 2 of 2 could not be transcribed: model exploded. Retry to try again."),
            "which part and why"
        );
        assert_eq!(s.text, "", "never a partial transcript");
        assert!(w.clipboard.writes.borrow().is_empty(), "nothing copied");

        let retried = d.retry(&p).unwrap();
        assert_eq!(retried.len(), 1, "only the failed part");
        let retrying = snap(&d, &w);
        assert_eq!(
            (retrying.phase, retrying.error, retrying.pending),
            (Phase::Transcribing, None, 1),
            "retrying"
        );
        finish(&mut d, &w, &retried[0]);
        assert_eq!(
            snap(&d, &w).text,
            "segment 0\n\nsegment 2",
            "the first part kept, the second redone"
        );
        assert_eq!(
            w.transcriber.calls.borrow()[2].0,
            16_000,
            "from the saved audio"
        );
    }

    #[test]
    fn retry_fails_again_when_the_saved_audio_is_gone_or_damaged() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Failed);
        w.files.fail_read.set(true);
        assert!(d.retry(&p).unwrap().is_empty(), "nothing to transcribe");
        let s = snap(&d, &w);
        assert_eq!(s.phase, Phase::Failed, "failed again");
        assert_eq!(
            s.error.as_deref(),
            Some(
                "Part 1 of 1 could not be transcribed: its audio could not be read: permission denied. Retry to try again."
            ),
            "why"
        );
        w.files.fail_read.set(false);
        w.files.put_segment(0, b"garbage".to_vec());
        assert!(d.retry(&p).unwrap().is_empty(), "nothing to transcribe");
        assert!(
            snap(&d, &w)
                .error
                .unwrap()
                .contains("its audio could not be read: not a WAV file"),
            "a damaged file"
        );
    }

    #[test]
    fn the_first_failure_is_the_one_reported() {
        let w = World::default();
        {
            let mut answers = w.transcriber.answers.borrow_mut();
            answers.push_back(Ok("fine".to_string()));
            answers.push_back(Err("first".to_string()));
            answers.push_back(Err("second".to_string()));
        }
        let p = w.ports();
        let (mut d, mut jobs) = at(&w, Phase::Paused);
        for _ in 0..2 {
            d.resume(&p).unwrap();
            jobs.push(d.pause(&p).unwrap().unwrap());
        }
        let _none = d.done(&p).unwrap();
        for job in &jobs {
            finish(&mut d, &w, job);
        }
        assert_eq!(
            snap(&d, &w).error.as_deref(),
            Some("Part 2 of 3 could not be transcribed: first. Retry to try again."),
            "the earliest failed part"
        );
    }

    #[test]
    fn late_duplicate_and_unknown_outcomes_are_ignored() {
        let w = World::default();
        let p = w.ports();
        let (mut d, old) = at(&w, Phase::Paused);
        let stale = w.run(&old[0]);
        d.discard(&p).unwrap();
        d.record(&p).unwrap();
        let job = d.done(&p).unwrap().unwrap();
        let _stale = d.complete(stale, &p);
        assert_eq!(
            snap(&d, &w).pending,
            1,
            "an outcome from a discarded session changes nothing"
        );

        let first = w.run(&job);
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Err("late".to_string()));
        let duplicate = w.run(&job);
        let _first = d.complete(first, &p);
        let _duplicate = d.complete(duplicate, &p);
        let s = snap(&d, &w);
        assert_eq!(
            (s.phase, s.text.as_str()),
            (Phase::Finished, "segment 1"),
            "the first answer stands"
        );
    }

    #[test]
    fn an_outcome_for_a_segment_that_does_not_exist_is_ignored() {
        let w = World::default();
        let p = w.ports();
        let (mut d, jobs) = at(&w, Phase::Paused);
        let outcome = w.run(&jobs[0]);
        d.discard(&p).unwrap();
        // A second dictation reaches the same session number with no
        // segment 0 yet.
        let mut other = idle(&w);
        other.new_session(&p).unwrap();
        let _unknown = other.complete(outcome, &p);
        assert_eq!(snap(&other, &w).segments, 0, "no segment appears");
    }

    #[test]
    fn discard_stops_a_recording_and_empties_everything() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Paused);
        d.resume(&p).unwrap();
        assert_eq!(w.files.segments(), 1, "a segment on disk");
        d.discard(&p).unwrap();
        assert_eq!(
            *w.recorder.calls.borrow(),
            ["start", "stop", "start", "stop"],
            "stopped"
        );
        assert!(w.files.segments() == 0, "audio deleted");
        let s = snap(&d, &w);
        assert_eq!(
            (s.phase, s.recorded_ms, s.segments, s.notice),
            (Phase::Ready, 0, 0, None),
            "an empty session"
        );
    }

    #[test]
    fn discard_while_paused_does_not_touch_the_microphone() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Paused);
        d.discard(&p).unwrap();
        assert_eq!(
            *w.recorder.calls.borrow(),
            ["start", "stop"],
            "no second stop"
        );
    }

    #[test]
    fn audio_that_cannot_be_deleted_blocks_new_and_discard() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Recording);
        w.files.fail_list.set(true);
        let e = d.discard(&p).unwrap_err();
        assert_eq!(
            e.to_string(),
            "Could not clear the last session's audio: permission denied",
            "sentence"
        );
        assert_eq!(snap(&d, &w).phase, Phase::Recording, "still recording");
        assert_eq!(
            *w.recorder.calls.borrow(),
            ["start"],
            "the recorder kept going"
        );
        let mut fresh = idle(&w);
        assert_eq!(
            fresh.new_session(&p),
            Err(DictationError::Store(PortError(
                "permission denied".to_string()
            ))),
            "new"
        );
        assert_eq!(snap(&fresh, &w).phase, Phase::Idle, "still idle");
    }

    #[test]
    fn new_deletes_the_last_sessions_audio() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Finished);
        assert_eq!(w.files.segments(), 1, "kept until the next session");
        d.new_session(&p).unwrap();
        assert!(w.files.segments() == 0, "gone");
        let s = snap(&d, &w);
        assert_eq!(
            (s.phase, s.text.as_str(), s.notice),
            (Phase::Ready, "", None),
            "empty and ready"
        );
    }

    #[test]
    fn a_clipboard_failure_is_reported_and_copy_tries_again_with_the_edits() {
        let w = World::default();
        w.clipboard.fail.set(true);
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Finished);
        let shown = snap(&d, &w);
        assert_eq!(shown.text, "segment 0", "the transcript is shown anyway");
        assert_eq!(
            shown.notice.as_deref(),
            Some("Could not copy to the clipboard: pasteboard unavailable. Use Copy to try again."),
            "said so"
        );
        assert_eq!(
            d.copy("segment zero", &p).unwrap_err().to_string(),
            "Could not copy to the clipboard: pasteboard unavailable",
            "copy fails too"
        );
        assert_eq!(snap(&d, &w).text, "segment 0", "unchanged");

        w.clipboard.fail.set(false);
        d.copy("segment zero", &p).unwrap();
        assert_eq!(
            *w.clipboard.writes.borrow(),
            ["segment zero"],
            "the edited text"
        );
        let copied = snap(&d, &w);
        assert_eq!(
            (copied.text.as_str(), copied.notice.as_deref()),
            ("segment zero", Some(COPIED)),
            "kept"
        );
    }

    #[test]
    fn launch_offers_retry_for_a_session_that_never_finished() {
        let w = World::default();
        w.files.set_vocabulary(Some("talos"));
        for i in [0u32, 2] {
            w.files.put_segment(i, encode_wav(&speech(1_600)));
        }
        let p = w.ports();
        let mut d = idle(&w);
        let s = snap(&d, &w);
        assert_eq!(
            (s.phase, s.segments),
            (Phase::Failed, 2),
            "failed, with both parts"
        );
        assert_eq!(
            s.error.as_deref(),
            Some(
                "The last session was not transcribed. Retry to transcribe its 2 recorded parts, or start a new session."
            ),
            "what to do"
        );
        let jobs = d.retry(&p).unwrap();
        assert_eq!(jobs.len(), 2, "both");
        for job in &jobs {
            finish(&mut d, &w, job);
        }
        assert_eq!(
            snap(&d, &w).text,
            "segment 0\n\nsegment 1",
            "transcribed in order"
        );
        assert_eq!(
            w.transcriber.calls.borrow()[0].1.as_deref(),
            Some(format!("{STYLE} Glossary: talos.").as_str()),
            "with the glossary"
        );
    }

    #[test]
    fn a_recovered_session_without_a_glossary_still_gets_the_style_sentence() {
        let w = World::default();
        w.files.set_vocabulary(None);
        for i in [0u32, 2] {
            w.files.put_segment(i, encode_wav(&speech(1_600)));
        }
        let p = w.ports();
        let mut d = idle(&w);
        let jobs = d.retry(&p).unwrap();
        for job in &jobs {
            finish(&mut d, &w, job);
        }
        assert_eq!(
            w.transcriber.calls.borrow()[0].1.as_deref(),
            Some(STYLE),
            "the style sentence alone"
        );
    }

    #[test]
    fn launch_names_a_single_leftover_part_in_the_singular() {
        let w = World::default();
        w.files.put_segment(0, encode_wav(&speech(10)));
        assert!(
            snap(&idle(&w), &w)
                .error
                .unwrap()
                .contains("its 1 recorded part,"),
            "singular"
        );
    }

    #[test]
    fn launch_with_an_unreadable_store_is_idle() {
        let w = World::default();
        w.files.put_segment(0, encode_wav(&speech(10)));
        w.files.fail_list.set(true);
        assert_eq!(snap(&idle(&w), &w).phase, Phase::Idle, "nothing to offer");
    }

    #[test]
    fn done_after_losing_the_only_part_says_nothing_was_heard() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Failed);
        d.discard(&p).unwrap();
        d.record(&p).unwrap();
        w.recorder
            .captures
            .borrow_mut()
            .push_back(Err("unplugged".to_string()));
        assert!(d.pause(&p).unwrap().is_none(), "lost");
        assert!(snap(&d, &w).notice.is_some(), "a notice");
        let _none = d.done(&p).unwrap();
        assert_eq!(
            snap(&d, &w).notice.as_deref(),
            Some(NOTHING_HEARD),
            "done replaces it"
        );
    }

    #[test]
    fn apply_queues_the_segments_an_action_closes_and_answers_with_the_snapshot() {
        let w = World::default();
        let p = w.ports();
        let mut d = idle(&w);
        let ready = d.apply(Action::New, "", &p).unwrap();
        assert_eq!(ready.phase, Phase::Ready, "the snapshot after the action");
        let _recording = d.apply(Action::Record, "", &p).unwrap();
        let paused = d.apply(Action::Pause, "", &p).unwrap();
        assert_eq!(
            (paused.phase, paused.pending),
            (Phase::Paused, 1),
            "paused, one in flight"
        );
        let _again = d.apply(Action::Resume, "", &p).unwrap();
        let waiting = d.apply(Action::Done, "", &p).unwrap();
        assert_eq!(waiting.phase, Phase::Transcribing, "waiting");
        let queued: Vec<_> = w.queue.jobs.borrow_mut().drain(..).collect();
        assert_eq!(queued.len(), 2, "both segments queued");
        for job in &queued {
            finish(&mut d, &w, job);
        }
        let copied = d.apply(Action::Copy, "my edit", &p).unwrap();
        assert_eq!(copied.text, "my edit", "copy takes the text");
        assert_eq!(
            d.apply(Action::Retry, "", &p),
            Err(DictationError::Illegal {
                action: Action::Retry,
                phase: Phase::Finished
            }),
            "errors pass through"
        );
    }

    #[test]
    fn a_segment_the_queue_refuses_fails_at_once_and_can_be_retried() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Recording);
        w.queue.fail.set(true);
        let failed = d.apply(Action::Done, "", &p).unwrap();
        assert_eq!(
            failed.phase,
            Phase::Failed,
            "nothing is left waiting forever"
        );
        assert_eq!(
            failed.error.as_deref(),
            Some(
                "Part 1 of 1 could not be transcribed: it could not be queued: the worker stopped. Retry to try again."
            ),
            "why"
        );
        w.queue.fail.set(false);
        let retrying = d.apply(Action::Retry, "", &p).unwrap();
        assert_eq!(
            (retrying.phase, retrying.pending),
            (Phase::Transcribing, 1),
            "queued again"
        );
        assert_eq!(w.queue.jobs.borrow().len(), 1, "on the queue");
    }

    #[test]
    fn the_revision_grows_with_every_change_and_only_then() {
        let w = World::default();
        let p = w.ports();
        let mut d = idle(&w);
        let ready = d.apply(Action::New, "", &p).unwrap();
        assert_eq!(ready.revision, 1, "an applied action");
        assert!(d.apply(Action::Pause, "", &p).is_err(), "refused");
        assert_eq!(snap(&d, &w).revision, 1, "a refused action changes nothing");
        let _recording = d.apply(Action::Record, "", &p).unwrap();
        let done = d.apply(Action::Done, "", &p).unwrap();
        assert_eq!(done.revision, 3, "one per action");
        let job = w.queue.jobs.borrow_mut().remove(0);
        let outcome = w.run(&job);
        let finished = d.complete(outcome.clone(), &p);
        assert_eq!(finished.revision, 4, "an accepted result");
        assert_eq!(
            d.complete(outcome, &p).revision,
            4,
            "an ignored result changes nothing"
        );
    }

    #[test]
    fn a_relaunch_after_a_finished_session_shows_its_transcript() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Finished);
        d.copy("edited", &p).unwrap();
        let relaunched = snap(&idle(&w), &w);
        assert_eq!(
            (
                relaunched.phase,
                relaunched.text.as_str(),
                relaunched.segments,
                relaunched.error
            ),
            (Phase::Finished, "edited", 1, None),
            "finished with the last (edited) text, not offered for Retry"
        );
        d.new_session(&p).unwrap();
        assert_eq!(snap(&idle(&w), &w).phase, Phase::Idle, "New deletes it");
    }

    #[test]
    fn a_silent_session_is_kept_as_finished_too() {
        let w = World::default();
        w.recorder
            .captures
            .borrow_mut()
            .push_back(Ok(Captured::new(vec![0.0; 1_600], 1, 16_000)));
        let p = w.ports();
        let (mut d, _) = at(&w, Phase::Recording);
        let job = d.done(&p).unwrap().unwrap();
        finish(&mut d, &w, &job);
        assert_eq!(
            snap(&idle(&w), &w).phase,
            Phase::Finished,
            "nothing to retry"
        );
    }

    /// New, Record, Pause, then Resume and Pause for each further part,
    /// Done, and every segment transcribed, all with the library on.
    fn library_session(w: &World, d: &mut Dictation, parts: usize) -> Snapshot {
        let p = w.library_ports();
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let mut jobs = vec![d.pause(&p).unwrap().unwrap()];
        for _ in 1..parts {
            d.resume(&p).unwrap();
            jobs.push(d.pause(&p).unwrap().unwrap());
        }
        assert!(d.done(&p).unwrap().is_none(), "paused");
        for job in &jobs {
            let _: Snapshot = d.complete(w.run(job), &p);
        }
        d.snapshot(&p)
    }

    fn library_ids(w: &World) -> Vec<String> {
        list(&w.files).into_iter().map(|e| e.id).collect()
    }

    #[test]
    fn with_the_library_off_nothing_is_kept_for_it() {
        let w = World::default();
        let p = w.ports();
        let mut d = idle(&w);
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let job = d.pause(&p).unwrap().unwrap();
        assert!(d.done(&p).unwrap().is_none(), "paused");
        let s = d.complete(w.run(&job), &p);
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.library_revision, 0, "no library change");
        let files = w.files.files.borrow();
        assert!(
            files.keys().all(|k| !k.starts_with("library/")),
            "nothing under library/"
        );
        assert!(!files.contains_key(STARTED), "no started.txt");
        assert!(w.queue.jobs.borrow().is_empty(), "no library job");
        assert_eq!(w.files.segments(), 1, "the segment stays");
    }

    #[test]
    fn the_entry_is_titled_by_the_first_record() {
        let w = World::default();
        w.clock
            .script(&[time(9, 0, 0), time(9, 5, 0), time(9, 10, 0)]);
        let mut d = idle(&w);
        let _: Snapshot = library_session(&w, &mut d, 2);
        assert_eq!(library_ids(&w), ["20261026-090000"], "T1");
        assert_eq!(
            list(&w.files).first().unwrap().title,
            "2026-10-26 09:00:00",
            "titled T1"
        );
        assert_eq!(
            w.files.read(STARTED).unwrap(),
            b"2026-10-26 09:00:00",
            "kept for a relaunch"
        );
    }

    #[test]
    fn discard_forgets_the_sessions_start() {
        let w = World::default();
        w.clock.script(&[time(9, 0, 0), time(9, 5, 0)]);
        let p = w.library_ports();
        let mut d = idle(&w);
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        d.discard(&p).unwrap();
        assert!(w.files.read(STARTED).is_err(), "started.txt is cleared");
        d.record(&p).unwrap();
        let job = d.pause(&p).unwrap().unwrap();
        assert!(d.done(&p).unwrap().is_none(), "paused");
        let _: Snapshot = d.complete(w.run(&job), &p);
        assert_eq!(library_ids(&w), ["20261026-090500"], "the new start");
    }

    #[test]
    fn a_session_start_that_cannot_be_kept_still_titles_the_entry() {
        let w = World::default();
        w.clock.script(&[time(9, 0, 0), time(9, 5, 0)]);
        let p = w.library_ports();
        let mut d = idle(&w);
        d.new_session(&p).unwrap();
        w.files.fail_write.set(true);
        d.record(&p).unwrap();
        w.files.fail_write.set(false);
        assert!(w.files.read(STARTED).is_err(), "not kept");
        let job = d.pause(&p).unwrap().unwrap();
        assert!(d.done(&p).unwrap().is_none(), "paused");
        let _: Snapshot = d.complete(w.run(&job), &p);
        assert_eq!(library_ids(&w), ["20261026-090000"], "still T1");
    }

    /// A failed session from a previous run, with `started` in started.txt.
    fn leftover(w: &World, started: Option<&[u8]>) -> Dictation {
        w.files.put_segment(0, encode_wav(&speech(16_000)));
        w.files.put_segment(1, encode_wav(&speech(8_000)));
        if let Some(bytes) = started {
            w.files.write(STARTED, bytes).unwrap();
        }
        let p = w.library_ports();
        let mut d = Dictation::recover(&p);
        let jobs = d.retry(&p).unwrap();
        for job in &jobs {
            let _: Snapshot = d.complete(w.run(job), &p);
        }
        d
    }

    #[test]
    fn a_recovered_session_is_titled_by_its_kept_start() {
        let w = World::default();
        w.clock.script(&[time(14, 0, 0)]);
        let d = leftover(&w, Some(b" 2026-10-26 08:00:00\n"));
        assert_eq!(library_ids(&w), ["20261026-080000"], "the kept start");
        let s = d.snapshot(&w.library_ports());
        assert_eq!(s.library_revision, 1, "archived once");
        let audio = decode_wav(
            &w.files
                .read(&entry_path("20261026-080000", "audio.wav"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(audio.len(), 24_000, "both parts");
    }

    #[test]
    fn a_damaged_or_missing_start_titles_by_the_clock() {
        for started in [
            None,
            Some(&b"garbage"[..]),
            Some(b"2026-10-26 08:00:0"),
            Some(b"2026-10-26 08:00:000"),
            Some(b"2026-10-26T08:00:00"),
            Some(b"2026-1a-26 08:00:00"),
            Some(&[0xff, 0xfe]),
        ] {
            let w = World::default();
            w.clock.script(&[time(14, 0, 0)]);
            let _: Dictation = leftover(&w, started);
            assert_eq!(
                library_ids(&w),
                ["20261026-140000"],
                "{started:?}: the time at archive"
            );
        }
    }

    #[test]
    fn the_library_revision_outlives_new() {
        let w = World::default();
        let mut d = idle(&w);
        assert_eq!(library_session(&w, &mut d, 1).library_revision, 1, "one");
        let s = library_session(&w, &mut d, 1);
        assert_eq!(s.library_revision, 2, "two");
        assert_eq!(library_ids(&w).len(), 2, "two entries");
    }

    #[test]
    fn a_relaunch_after_an_archive_is_finished_with_no_parts() {
        let w = World::default();
        let mut d = idle(&w);
        let done = library_session(&w, &mut d, 2);
        let p = w.library_ports();
        let again = Dictation::recover(&p);
        let s = again.snapshot(&p);
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.text, done.text, "the transcript");
        assert_eq!(s.segments, 0, "no parts left");
        assert_eq!(s.library_revision, 0, "a fresh count");
        assert_eq!(library_ids(&w).len(), 1, "not archived again");
    }

    #[test]
    fn a_queue_that_refuses_the_library_jobs_still_archives() {
        let w = World::default();
        let mut d = idle(&w);
        let p = w.library_ports();
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let job = d.pause(&p).unwrap().unwrap();
        assert!(d.done(&p).unwrap().is_none(), "paused");
        w.queue.fail.set(true);
        let s = d.complete(w.run(&job), &p);
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.notice.as_deref(), Some(COPIED), "copied");
        assert_eq!(s.library_revision, 1, "archived");
        assert_eq!(
            list(&w.files).first().unwrap().summary,
            "segment 0",
            "with the excerpt"
        );
        assert!(
            w.files
                .read(&entry_path("20261026-140309", "audio.wav"))
                .is_ok(),
            "and its audio"
        );
        assert!(w.queue.jobs.borrow().is_empty(), "nothing queued");
    }

    const CID: &str = "20261026-140309";

    const CLAWED: &str = "Ask Clawed to check the board.";

    const CLAUDE: &str = "Ask Claude to check the board.";

    fn cleanup_on(w: &World) {
        w.files.write(SETTINGS, br#"{"cleanup":true}"#).unwrap();
    }

    /// A one-part session heard as `heard`, driven through Done and its
    /// transcription with `p`, so that the cleanup job (if any) is queued.
    fn heard_with(w: &World, p: &Ports<'_>, heard: &str) -> (Dictation, Snapshot) {
        let mut d = idle(w);
        d.new_session(p).unwrap();
        d.record(p).unwrap();
        let job = d.pause(p).unwrap().unwrap();
        assert!(d.done(p).unwrap().is_none(), "paused");
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Ok(heard.to_string()));
        let s = d.complete(w.run(&job), p);
        (d, s)
    }

    /// With cleanup and the library on, a session heard as `heard` up to its
    /// queued cleanup job, which is taken off the queue.
    fn to_cleanup(w: &World, heard: &str) -> (Dictation, Job) {
        cleanup_on(w);
        let (d, s) = heard_with(w, &w.library_cleaner_ports(), heard);
        assert_eq!(s.phase, Phase::Transcribing, "waiting for the cleanup");
        let job = w.queue.jobs.borrow_mut().pop().unwrap();
        assert!(format!("{job:?}").contains("Cleanup"), "{job:?}");
        (d, job)
    }

    /// Runs the cleanup job with the cleaner answering `reply`.
    fn cleaned(w: &World, d: &mut Dictation, job: &Job, reply: Result<&str, &str>) -> Snapshot {
        let p = w.library_cleaner_ports();
        w.cleaner
            .answers
            .borrow_mut()
            .push_back(reply.map(str::to_string).map_err(str::to_string));
        d.complete(w.run_with(job, &p), &p)
    }

    fn entry_file(w: &World, name: &str) -> Option<String> {
        w.files
            .read(&entry_path(CID, name))
            .ok()
            .map(|b| String::from_utf8(b).unwrap())
    }

    fn entry_json(w: &World) -> Value {
        serde_json::from_str(&entry_file(w, "entry.json").unwrap()).unwrap()
    }

    #[test]
    fn with_cleanup_off_the_session_finishes_at_once() {
        let w = World::default();
        let (_, s) = heard_with(&w, &w.cleaner_ports(), CLAWED);
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.text, CLAWED, "the raw text");
        assert!(
            w.cleaner.calls.borrow().is_empty(),
            "the model is not asked"
        );
        assert!(w.queue.jobs.borrow().is_empty(), "nothing queued");
    }

    #[test]
    fn with_cleanup_on_and_no_model_the_session_finishes_at_once() {
        let w = World::default();
        cleanup_on(&w);
        let (_, s) = heard_with(&w, &w.ports(), CLAWED);
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.text, CLAWED, "the raw text");
        assert!(w.queue.jobs.borrow().is_empty(), "nothing queued");
    }

    #[test]
    fn with_cleanup_on_the_session_waits_for_the_model() {
        let w = World::default();
        w.files.set_vocabulary(Some("Claude\n"));
        cleanup_on(&w);
        let p = w.library_cleaner_ports();
        let (d, s) = heard_with(&w, &p, CLAWED);
        assert_eq!(s.phase, Phase::Transcribing, "still transcribing");
        assert_eq!(s.pending, 0, "no segment pending");
        assert_eq!(s.text, "", "no text yet");
        assert!(w.clipboard.writes.borrow().is_empty(), "nothing copied");
        assert_eq!(s, d.snapshot(&p), "complete answers with the snapshot");
        let jobs = w.queue.jobs.borrow().clone();
        assert_eq!(jobs.len(), 1, "one job: {jobs:?}");
        let job = jobs.first().unwrap();
        assert!(format!("{job:?}").contains("Cleanup"), "{job:?}");
        let _outcome = w.run_with(job, &p);
        let calls = w.cleaner.calls.borrow();
        let (system, request) = calls.first().unwrap();
        assert_eq!(system, PROMPT, "the system prompt");
        assert!(
            request.starts_with("Glossary:\nClaude\n\nTranscript:\n"),
            "{request:?}"
        );
    }

    #[test]
    fn an_accepted_cleanup_is_copied_and_kept_beside_the_raw_text() {
        let w = World::default();
        w.files.set_vocabulary(Some("Claude\n"));
        let (mut d, job) = to_cleanup(&w, CLAWED);
        let s = cleaned(&w, &mut d, &job, Ok(CLAUDE));
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.text, CLAUDE, "the cleaned text");
        assert_eq!(s.notice.as_deref(), Some(COPIED), "copied");
        assert_eq!(*w.clipboard.writes.borrow(), [CLAUDE], "on the clipboard");
        assert_eq!(
            w.files.read(TRANSCRIPT).unwrap(),
            CLAUDE.as_bytes(),
            "kept for a relaunch"
        );
        assert_eq!(
            entry_file(&w, "transcript.txt").as_deref(),
            Some(CLAUDE),
            "the entry"
        );
        assert_eq!(
            entry_file(&w, "transcript.raw.txt").as_deref(),
            Some(CLAWED),
            "the raw text"
        );
        assert_eq!(entry_file(&w, "cleanup.reply.txt"), None, "no reply kept");
        assert_eq!(
            entry_json(&w).get("cleanup"),
            Some(&json!("cleaned")),
            "label"
        );
        assert_eq!(entry_json(&w).get("cleanup_error"), None, "no error");
        let summary = w.queue.jobs.borrow_mut().pop().unwrap();
        assert!(format!("{summary:?}").contains("Summary"), "{summary:?}");
        let _outcome = w.run_with(&summary, &w.library_cleaner_ports());
        assert_eq!(
            *w.summarizer.transcripts.borrow(),
            [CLAUDE],
            "the cleaned text is summarized"
        );
    }

    const SHORTCUT: &str = "Press command-backslash to toggle the library.";

    const SHORTCUT_GLYPHS: &str = "Press ⌘\\ to toggle the library.";

    #[test]
    fn shortcuts_are_written_as_glyphs_with_cleanup_off() {
        let w = World::default();
        let (_, s) = heard_with(&w, &w.library_ports(), SHORTCUT);
        assert_eq!(s.text, SHORTCUT_GLYPHS, "snapshot");
        assert_eq!(*w.clipboard.writes.borrow(), [SHORTCUT_GLYPHS], "clipboard");
        assert_eq!(
            w.files.read(TRANSCRIPT).unwrap(),
            SHORTCUT_GLYPHS.as_bytes(),
            "kept for a relaunch"
        );
        assert_eq!(
            entry_file(&w, "transcript.txt").as_deref(),
            Some(SHORTCUT_GLYPHS),
            "entry"
        );
        assert_eq!(
            entry_file(&w, "transcript.raw.txt").as_deref(),
            Some(SHORTCUT),
            "as spoken"
        );
        let json = entry_json(&w);
        assert_eq!(json.get("cleanup"), Some(&json!("off")), "label");
        assert_eq!(
            json.get("summary"),
            Some(&json!(scriber_core::library::excerpt(SHORTCUT_GLYPHS))),
            "summary"
        );
        assert_eq!(
            json.get("summary_source"),
            Some(&json!("excerpt")),
            "source"
        );
    }

    #[test]
    fn shortcuts_are_written_after_an_unchanged_reply() {
        let w = World::default();
        let (mut d, job) = to_cleanup(&w, SHORTCUT);
        let s = cleaned(&w, &mut d, &job, Ok(SHORTCUT));
        assert_eq!(s.text, SHORTCUT_GLYPHS, "text");
        assert_eq!(
            entry_json(&w).get("cleanup"),
            Some(&json!("unchanged")),
            "label"
        );
        assert_eq!(
            entry_file(&w, "transcript.raw.txt").as_deref(),
            Some(SHORTCUT),
            "as spoken"
        );
    }

    #[test]
    fn shortcuts_are_written_after_a_cleaned_reply_and_the_request_is_raw() {
        let w = World::default();
        w.files.set_vocabulary(Some("Claude\n"));
        let (mut d, job) = to_cleanup(&w, "Ask Clawed to press command comma.");
        let s = cleaned(&w, &mut d, &job, Ok("Ask Claude to press command comma."));
        assert_eq!(s.text, "Ask Claude to press ⌘,.", "text");
        assert_eq!(
            entry_file(&w, "transcript.raw.txt").as_deref(),
            Some("Ask Clawed to press command comma."),
            "Whisper's text"
        );
        assert_eq!(
            entry_json(&w).get("cleanup"),
            Some(&json!("cleaned")),
            "label"
        );
        assert!(
            w.cleaner.calls.borrow()[0].1.contains("command comma"),
            "the request was unconverted"
        );
    }

    #[test]
    fn shortcuts_are_written_after_a_rejected_reply() {
        let w = World::default();
        let (mut d, job) = to_cleanup(&w, SHORTCUT);
        let reply = format!("Here is the text: {SHORTCUT}");
        let s = cleaned(&w, &mut d, &job, Ok(&reply));
        assert_eq!(s.text, SHORTCUT_GLYPHS, "text");
        assert_eq!(
            entry_file(&w, "cleanup.reply.txt").as_deref(),
            Some(reply.as_str()),
            "reply verbatim"
        );
        assert_eq!(
            entry_file(&w, "transcript.raw.txt").as_deref(),
            Some(SHORTCUT),
            "as spoken"
        );
        assert_eq!(
            entry_json(&w).get("cleanup"),
            Some(&json!("rejected-new-words")),
            "label"
        );
    }

    #[test]
    fn shortcuts_are_written_after_a_failed_model() {
        let w = World::default();
        let (mut d, job) = to_cleanup(&w, SHORTCUT);
        let s = cleaned(&w, &mut d, &job, Err("down"));
        assert_eq!(s.text, SHORTCUT_GLYPHS, "text");
        assert_eq!(
            entry_json(&w).get("cleanup"),
            Some(&json!("failed")),
            "label"
        );
        assert_eq!(
            entry_file(&w, "transcript.raw.txt").as_deref(),
            Some(SHORTCUT),
            "as spoken"
        );
    }

    #[test]
    fn shortcuts_are_written_when_the_queue_refuses_the_cleanup() {
        let w = World::default();
        cleanup_on(&w);
        let p = w.library_cleaner_ports();
        let mut d = idle(&w);
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let job = d.pause(&p).unwrap().unwrap();
        assert!(d.done(&p).unwrap().is_none(), "paused");
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Ok(SHORTCUT.to_string()));
        w.queue.fail.set(true);
        let s = d.complete(w.run(&job), &p);
        assert_eq!(s.text, SHORTCUT_GLYPHS, "text");
        assert_eq!(*w.clipboard.writes.borrow(), [SHORTCUT_GLYPHS], "clipboard");
        assert_eq!(
            entry_json(&w).get("cleanup"),
            Some(&json!("failed")),
            "label"
        );
        assert_eq!(
            entry_file(&w, "transcript.raw.txt").as_deref(),
            Some(SHORTCUT),
            "as spoken"
        );
    }

    #[test]
    fn the_summary_job_gets_the_glyphs() {
        let w = World::default();
        w.files
            .write(SETTINGS, br#"{"summaries":true,"summary_model":"late:1b"}"#)
            .unwrap();
        let _heard = heard_with(&w, &w.library_ports(), SHORTCUT);
        let summary = w
            .queue
            .jobs
            .borrow_mut()
            .iter()
            .find(|j| format!("{j:?}").contains("Summary"))
            .cloned()
            .unwrap();
        let _outcome = w.run_with(&summary, &w.library_ports());
        assert_eq!(
            *w.summarizer.transcripts.borrow(),
            [SHORTCUT_GLYPHS],
            "summarized with glyphs"
        );
    }

    #[test]
    fn a_retried_session_writes_the_glyphs() {
        let w = World::default();
        let p = w.library_ports();
        let mut d = idle(&w);
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let seg = d.pause(&p).unwrap().unwrap();
        assert!(d.done(&p).unwrap().is_none(), "paused");
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Err("model exploded".to_string()));
        assert_eq!(d.complete(w.run(&seg), &p).phase, Phase::Failed, "failed");
        let retry = d.retry(&p).unwrap();
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Ok(SHORTCUT.to_string()));
        let s = d.complete(w.run(retry.first().unwrap()), &p);
        assert_eq!(s.text, SHORTCUT_GLYPHS, "text");
    }

    #[test]
    fn copy_never_converts() {
        let w = World::default();
        let p = w.library_ports();
        let (mut d, _) = heard_with(&w, &p, SHORTCUT);
        d.copy("command-k", &p).unwrap();
        assert_eq!(d.snapshot(&p).text, "command-k", "kept as edited");
        assert_eq!(
            w.clipboard.writes.borrow().last().map(String::as_str),
            Some("command-k"),
            "clipboard"
        );
    }

    #[test]
    fn an_unchanged_reply_keeps_no_raw_text() {
        let w = World::default();
        let (mut d, job) = to_cleanup(&w, CLAWED);
        let s = cleaned(&w, &mut d, &job, Ok(CLAWED));
        assert_eq!(s.text, CLAWED, "the text");
        assert_eq!(
            entry_json(&w).get("cleanup"),
            Some(&json!("unchanged")),
            "label"
        );
        assert_eq!(entry_file(&w, "transcript.raw.txt"), None, "no raw text");
    }

    #[test]
    fn a_rejected_reply_falls_back_to_the_raw_text_and_is_kept() {
        let w = World::default();
        let (mut d, job) = to_cleanup(&w, CLAWED);
        let reply = format!("Here is the text: {CLAWED}");
        let s = cleaned(&w, &mut d, &job, Ok(&reply));
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.text, CLAWED, "the raw text");
        assert_eq!(s.error, None, "no error");
        assert_eq!(s.notice.as_deref(), Some(COPIED), "copied as usual");
        assert_eq!(*w.clipboard.writes.borrow(), [CLAWED], "on the clipboard");
        assert_eq!(
            entry_file(&w, "cleanup.reply.txt").as_deref(),
            Some(reply.as_str()),
            "the reply"
        );
        assert_eq!(
            entry_json(&w).get("cleanup"),
            Some(&json!("rejected-new-words")),
            "label"
        );
        assert_eq!(entry_file(&w, "transcript.raw.txt"), None, "no raw text");
    }

    #[test]
    fn a_failed_model_falls_back_to_the_raw_text() {
        let w = World::default();
        let (mut d, job) = to_cleanup(&w, CLAWED);
        let s = cleaned(&w, &mut d, &job, Err("down"));
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.text, CLAWED, "the raw text");
        assert_eq!(s.error, None, "no error");
        assert_eq!(
            entry_json(&w).get("cleanup"),
            Some(&json!("failed")),
            "label"
        );
        assert_eq!(
            entry_json(&w).get("cleanup_error"),
            Some(&json!("down")),
            "why"
        );
        assert_eq!(entry_file(&w, "cleanup.reply.txt"), None, "no reply");
        assert_eq!(entry_file(&w, "transcript.raw.txt"), None, "no raw text");
    }

    #[test]
    fn a_queue_that_refuses_the_cleanup_finishes_with_the_raw_text() {
        let w = World::default();
        cleanup_on(&w);
        let p = w.library_cleaner_ports();
        let mut d = idle(&w);
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let job = d.pause(&p).unwrap().unwrap();
        assert!(d.done(&p).unwrap().is_none(), "paused");
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Ok(CLAWED.to_string()));
        w.queue.fail.set(true);
        let s = d.complete(w.run(&job), &p);
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.text, CLAWED, "the raw text");
        assert_eq!(*w.clipboard.writes.borrow(), [CLAWED], "copied");
        assert_eq!(
            entry_json(&w).get("cleanup"),
            Some(&json!("failed")),
            "label"
        );
        assert!(w.cleaner.calls.borrow().is_empty(), "never asked");
    }

    #[test]
    fn with_cleanup_off_the_entry_says_so() {
        let w = World::default();
        let (_, s) = heard_with(&w, &w.library_cleaner_ports(), CLAWED);
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(entry_json(&w).get("cleanup"), Some(&json!("off")), "label");
        assert!(
            !entry_file(&w, "entry.json")
                .unwrap()
                .contains("cleanup_error"),
            "no cleanup_error key"
        );
    }

    #[test]
    fn a_silent_session_is_not_cleaned_up() {
        let w = World::default();
        cleanup_on(&w);
        let (_, s) = heard_with(&w, &w.library_cleaner_ports(), "");
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.notice.as_deref(), Some(NOTHING_HEARD), "nothing heard");
        assert!(w.queue.jobs.borrow().is_empty(), "no cleanup queued");
    }

    #[test]
    fn a_stale_or_late_cleanup_changes_nothing() {
        let w = World::default();
        let p = w.library_cleaner_ports();
        let (mut d, first) = to_cleanup(&w, CLAWED);
        let first = w.run_with(&first, &p);
        let _: Snapshot = d.complete(first.clone(), &p);
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let job = d.pause(&p).unwrap().unwrap();
        assert!(d.done(&p).unwrap().is_none(), "paused");
        let _: Snapshot = d.complete(w.run(&job), &p);
        let second = w.queue.jobs.borrow_mut().pop().unwrap();
        let before = d.snapshot(&p);
        assert_eq!(before.phase, Phase::Transcribing, "cleaning again");
        assert_eq!(
            d.complete(first, &p),
            before,
            "an outcome from the last session"
        );
        let second = w.run_with(&second, &p);
        let finished = d.complete(second.clone(), &p);
        assert_eq!(finished.phase, Phase::Finished, "finished");
        assert_eq!(d.complete(second, &p), finished, "a late duplicate");
    }

    #[test]
    fn a_cleanup_job_without_a_model_fails_closed() {
        let w = World::default();
        let p = w.library_cleaner_ports();
        let (mut d, job) = to_cleanup(&w, CLAWED);
        let outcome = w.run_with(&job, &w.library_ports());
        assert!(
            format!("{outcome:?}").contains(r#"result: Err("the cleanup model is off")"#),
            "{outcome:?}"
        );
        let s = d.complete(outcome.clone(), &p);
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.text, CLAWED, "the raw text");
        assert_eq!(
            entry_json(&w).get("cleanup_error"),
            Some(&json!("the cleanup model is off")),
            "why"
        );
        // A later Retry's transcription, with no cleanup running yet.
        let mut d2 = Dictation::default();
        d2.new_session(&p).unwrap();
        d2.record(&p).unwrap();
        let seg = d2.pause(&p).unwrap().unwrap();
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Err("model exploded".to_string()));
        assert!(d2.done(&p).unwrap().is_none(), "paused");
        let _: Snapshot = d2.complete(w.run(&seg), &p);
        let retry = d2.retry(&p).unwrap();
        let before = d2.snapshot(&p);
        assert_eq!(before.phase, Phase::Transcribing, "retrying");
        assert_eq!(retry.len(), 1, "one part");
        assert_eq!(d2.complete(outcome, &p), before, "no cleanup is running");
    }

    #[test]
    fn retry_goes_through_the_cleanup_too() {
        let w = World::default();
        cleanup_on(&w);
        let p = w.cleaner_ports();
        let mut d = idle(&w);
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let seg = d.pause(&p).unwrap().unwrap();
        assert!(d.done(&p).unwrap().is_none(), "paused");
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Err("model exploded".to_string()));
        assert_eq!(d.complete(w.run(&seg), &p).phase, Phase::Failed, "failed");
        let retry = d.retry(&p).unwrap();
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Ok(CLAWED.to_string()));
        let s = d.complete(w.run(retry.first().unwrap()), &p);
        assert_eq!(s.phase, Phase::Transcribing, "cleaning");
        let job = w.queue.jobs.borrow_mut().pop().unwrap();
        w.cleaner.answers.borrow_mut().push_back(Ok(
            "Ask Claude Claude Claude Claude to check the board.".to_string(),
        ));
        w.files.set_vocabulary(Some("Claude"));
        let done = d.complete(w.run_with(&job, &p), &p);
        assert_eq!(done.phase, Phase::Finished, "finished");
        assert_eq!(
            done.text, CLAWED,
            "Claude was not a term when the part was heard"
        );
        assert_eq!(w.cleaner.calls.borrow().len(), 1, "asked once");
    }

    #[test]
    fn at_most_two_hundred_terms_are_sent() {
        let w = World::default();
        let terms: Vec<String> = (1..=201).map(|n| format!("t{n}")).collect();
        w.files.set_vocabulary(Some(&terms.join("\n")));
        let (_, job) = to_cleanup(&w, CLAWED);
        let _outcome = w.run_with(&job, &w.library_cleaner_ports());
        let calls = w.cleaner.calls.borrow();
        let (_, request) = calls.first().unwrap();
        assert!(request.contains("\nt200\n"), "the 200th term");
        assert!(!request.contains("t201"), "not the 201st");
    }

    #[test]
    fn a_quit_during_cleanup_leaves_the_session_to_retry() {
        let w = World::default();
        let (_, _job) = to_cleanup(&w, CLAWED);
        let s = snap(&Dictation::recover(&w.ports()), &w);
        assert_eq!(s.phase, Phase::Failed, "failed");
        assert_eq!(
            s.error.as_deref(),
            Some(
                "The last session was not transcribed. Retry to transcribe its 1 recorded part, or start a new session."
            ),
            "retry offered"
        );
        assert_eq!(w.files.segments(), 1, "the part is kept");
    }

    #[test]
    fn a_raw_text_that_cannot_be_kept_fails_the_archive() {
        let w = World::default();
        w.files.set_vocabulary(Some("Claude"));
        let (mut d, job) = to_cleanup(&w, CLAWED);
        *w.files.fail_write_suffix.borrow_mut() = Some("transcript.raw.txt".to_string());
        let s = cleaned(&w, &mut d, &job, Ok(CLAUDE));
        assert_eq!(s.phase, Phase::Finished, "finished");
        assert_eq!(s.text, CLAUDE, "the cleaned text");
        assert_eq!(entry_file(&w, "entry.json"), None, "no entry");
        assert_eq!(w.files.segments(), 1, "the part stays");
        assert_eq!(s.library_revision, 0, "the library is unchanged");
    }

    /// A one-part library session up to its queued summary job, which is
    /// taken off the queue.
    fn to_summary(w: &World) -> Job {
        let mut d = idle(w);
        let _: Snapshot = library_session(w, &mut d, 1);
        let mut jobs = w.queue.jobs.borrow_mut();
        let at = jobs
            .iter()
            .position(|j| format!("{j:?}").contains("Summary"))
            .unwrap();
        jobs.remove(at)
    }

    fn summary_model_in_entry(w: &World) -> Option<Value> {
        let id = library_ids(w).into_iter().next().unwrap();
        let bytes = w.files.read(&entry_path(&id, "entry.json")).unwrap();
        let entry: Value = serde_json::from_slice(&bytes).unwrap();
        entry.get("summary_model").cloned()
    }

    #[test]
    fn with_no_model_saved_the_summary_asks_for_the_default() {
        let w = World::default();
        let job = to_summary(&w);
        let _outcome = w.run_with(&job, &w.library_ports());
        assert_eq!(*w.summarizer.models.borrow(), [""], "the default");
        assert_eq!(summary_model_in_entry(&w), Some(json!("")), "recorded");
    }

    #[test]
    fn the_saved_summary_model_is_asked_and_recorded() {
        let w = World::default();
        w.files
            .write(SETTINGS, br#"{"summary_model":"gemma3:4b"}"#)
            .unwrap();
        let job = to_summary(&w);
        let _outcome = w.run_with(&job, &w.library_ports());
        assert_eq!(*w.summarizer.models.borrow(), ["gemma3:4b"], "asked");
        assert_eq!(
            summary_model_in_entry(&w),
            Some(json!("gemma3:4b")),
            "recorded"
        );
    }

    #[test]
    fn the_summary_model_is_read_when_the_job_runs() {
        let w = World::default();
        let job = to_summary(&w);
        w.files
            .write(SETTINGS, br#"{"summaries":true,"summary_model":"late:1b"}"#)
            .unwrap();
        let _outcome = w.run_with(&job, &w.library_ports());
        assert_eq!(*w.summarizer.models.borrow(), ["late:1b"], "asked");
        assert_eq!(
            summary_model_in_entry(&w),
            Some(json!("late:1b")),
            "recorded"
        );
    }

    #[test]
    fn cleanup_and_summary_each_ask_their_own_model() {
        let w = World::default();
        w.files
            .write(
                SETTINGS,
                br#"{"cleanup":true,"cleanup_model":"gemma3:4b","summary_model":"qwen3:8b"}"#,
            )
            .unwrap();
        let p = w.library_cleaner_ports();
        let (mut d, _) = heard_with(&w, &p, CLAWED);
        let cleanup = w.queue.jobs.borrow_mut().pop().unwrap();
        assert!(format!("{cleanup:?}").contains("Cleanup"), "{cleanup:?}");
        let _: Snapshot = d.complete(w.run_with(&cleanup, &p), &p);
        let summary = w.queue.jobs.borrow_mut().pop().unwrap();
        assert!(format!("{summary:?}").contains("Summary"), "{summary:?}");
        let _outcome = w.run_with(&summary, &p);
        assert_eq!(*w.cleaner.models.borrow(), ["gemma3:4b"], "cleanup");
        assert_eq!(*w.summarizer.models.borrow(), ["qwen3:8b"], "summary");
    }

    #[test]
    fn the_cleanup_model_is_read_when_the_job_runs() {
        let w = World::default();
        let (_, job) = to_cleanup(&w, CLAWED);
        w.files
            .write(SETTINGS, br#"{"cleanup":true,"cleanup_model":"late:1b"}"#)
            .unwrap();
        let _outcome = w.run_with(&job, &w.library_cleaner_ports());
        assert_eq!(*w.cleaner.models.borrow(), ["late:1b"], "asked");
    }

    // The microphone: which device each Record and Resume opens, the
    // fallback to the Mac default, and the decision log.

    fn save_input(w: &World, name: &str) {
        let bytes = serde_json::to_vec(&json!({ "input_device": name })).unwrap();
        w.files.write(SETTINGS, &bytes).unwrap();
    }

    fn opened(w: &World) -> Vec<String> {
        w.recorder.opened.borrow().clone()
    }

    fn log_lines(w: &World) -> Vec<String> {
        w.files
            .read(MICROPHONE_LOG)
            .map(|b| String::from_utf8(b).unwrap())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn start_lines(w: &World) -> usize {
        log_lines(w)
            .iter()
            .filter(|l| l.starts_with(r#"{"event":"start""#))
            .count()
    }

    #[test]
    fn the_fallback_sentence_is_pinned() {
        assert_eq!(
            input_fallback("Jabra Evolve 65", "MacBook Pro Microphone"),
            "Jabra Evolve 65 is not available, so this part is recording from the Mac default, MacBook Pro Microphone.",
            "sentence"
        );
    }

    #[test]
    fn the_log_path_is_pinned() {
        assert_eq!(MICROPHONE_LOG, "microphone-log.jsonl", "path");
    }

    #[test]
    fn with_no_choice_record_opens_the_mac_default() {
        let w = World::default();
        let (d, _) = at(&w, Phase::Recording);
        assert_eq!(opened(&w), [""], "asked for the default");
        let s = snap(&d, &w);
        assert_eq!(s.notice, None, "no notice");
        assert_eq!(s.microphone.as_deref(), Some("Fake Mic"), "opened");
        assert_eq!(start_lines(&w), 1, "one start line");
    }

    #[test]
    fn a_chosen_device_is_opened() {
        let w = World::default();
        save_input(&w, "AirPods Pro");
        let (d, _) = at(&w, Phase::Recording);
        assert_eq!(opened(&w), ["AirPods Pro"], "asked for the choice");
        let s = snap(&d, &w);
        assert_eq!(s.notice, None, "no notice");
        assert_eq!(s.microphone.as_deref(), Some("AirPods Pro"), "opened");
        assert_eq!(start_lines(&w), 1, "one start line");
    }

    #[test]
    fn a_missing_device_falls_back_to_the_mac_default_and_says_so() {
        let w = World::default();
        save_input(&w, "Jabra");
        w.recorder
            .unavailable
            .borrow_mut()
            .push("Jabra".to_string());
        let (d, _) = at(&w, Phase::Recording);
        assert_eq!(opened(&w), ["Jabra", ""], "then the default");
        let s = snap(&d, &w);
        assert_eq!(s.phase, Phase::Recording, "recording");
        assert_eq!(s.microphone.as_deref(), Some("Fake Mic"), "the default");
        assert_eq!(
            s.notice,
            Some(input_fallback("Jabra", "Fake Mic")),
            "says so"
        );
        assert_eq!(
            w.files.read(SETTINGS).unwrap(),
            br#"{"input_device":"Jabra"}"#,
            "the choice is kept"
        );
        assert_eq!(start_lines(&w), 1, "one start line");
    }

    #[test]
    fn record_is_refused_only_when_the_mac_default_fails_too() {
        let w = World::default();
        save_input(&w, "Jabra");
        w.recorder
            .unavailable
            .borrow_mut()
            .push("Jabra".to_string());
        *w.recorder.start_error.borrow_mut() = Some("no mic".to_string());
        let (mut d, _) = at(&w, Phase::Ready);
        let before = snap(&d, &w);
        assert_eq!(
            d.record(&w.ports()),
            Err(DictationError::Recorder(PortError("no mic".to_string()))),
            "the second error"
        );
        assert_eq!(*w.recorder.calls.borrow(), ["start", "start"], "two tries");
        assert_eq!(snap(&d, &w), before, "unchanged");
        assert_eq!(start_lines(&w), 1, "one start line");
    }

    #[test]
    fn resume_is_refused_only_when_the_mac_default_fails_too() {
        let w = World::default();
        save_input(&w, "AirPods Pro");
        let (mut d, _) = at(&w, Phase::Paused);
        save_input(&w, "Jabra");
        w.recorder
            .unavailable
            .borrow_mut()
            .push("Jabra".to_string());
        *w.recorder.start_error.borrow_mut() = Some("no mic".to_string());
        let before = snap(&d, &w);
        assert_eq!(
            d.resume(&w.ports()),
            Err(DictationError::Recorder(PortError("no mic".to_string()))),
            "the second error"
        );
        assert_eq!(
            *w.recorder.calls.borrow(),
            ["start", "stop", "start", "start"],
            "two tries"
        );
        let s = snap(&d, &w);
        assert_eq!(s, before, "unchanged");
        assert_eq!(s.phase, Phase::Paused, "paused");
        assert_eq!(s.microphone.as_deref(), Some("AirPods Pro"), "the earlier");
        assert_eq!(start_lines(&w), 2, "one start line per attempt");
    }

    #[test]
    fn with_no_choice_a_failing_default_is_tried_once() {
        let w = World::default();
        *w.recorder.start_error.borrow_mut() = Some("no mic".to_string());
        let (mut d, _) = at(&w, Phase::Ready);
        assert_eq!(
            d.record(&w.ports()),
            Err(DictationError::Recorder(PortError("no mic".to_string()))),
            "refused"
        );
        assert_eq!(*w.recorder.calls.borrow(), ["start"], "one try");
        assert_eq!(snap(&d, &w).microphone, None, "nothing opened");
        assert_eq!(start_lines(&w), 1, "one start line");
    }

    #[test]
    fn a_choice_made_while_paused_applies_at_resume() {
        let w = World::default();
        save_input(&w, "AirPods Pro");
        let (mut d, jobs) = at(&w, Phase::Paused);
        save_input(&w, "MacBook Pro Microphone");
        assert_eq!(*w.recorder.calls.borrow(), ["start", "stop"], "not yet");
        for job in &jobs {
            finish(&mut d, &w, job);
        }
        assert_eq!(
            *w.recorder.calls.borrow(),
            ["start", "stop"],
            "the job leaves it"
        );
        d.resume(&w.ports()).unwrap();
        assert_eq!(
            opened(&w),
            ["AirPods Pro", "MacBook Pro Microphone"],
            "the new choice"
        );
        assert_eq!(
            snap(&d, &w).microphone.as_deref(),
            Some("MacBook Pro Microphone"),
            "opened"
        );
        assert_eq!(start_lines(&w), 2, "one start line per attempt");
    }

    #[test]
    fn the_fallback_notice_goes_at_pause_and_returns_at_a_resume_that_falls_back() {
        let w = World::default();
        save_input(&w, "Jabra");
        w.recorder
            .unavailable
            .borrow_mut()
            .push("Jabra".to_string());
        let (mut d, _) = at(&w, Phase::Paused);
        assert_eq!(snap(&d, &w).notice, None, "gone at pause");
        d.resume(&w.ports()).unwrap();
        assert_eq!(
            snap(&d, &w).notice,
            Some(input_fallback("Jabra", "Fake Mic")),
            "set again"
        );
    }

    #[test]
    fn the_microphone_follows_the_session() {
        let w = World::default();
        let p = w.ports();
        let mut d = idle(&w);
        d.new_session(&p).unwrap();
        assert_eq!(snap(&d, &w).microphone, None, "before the first record");
        save_input(&w, "AirPods Pro");
        d.record(&p).unwrap();
        assert_eq!(
            snap(&d, &w).microphone.as_deref(),
            Some("AirPods Pro"),
            "record"
        );
        let _job = d.pause(&p).unwrap();
        assert_eq!(
            snap(&d, &w).microphone.as_deref(),
            Some("AirPods Pro"),
            "paused"
        );
        save_input(&w, "");
        d.resume(&p).unwrap();
        assert_eq!(
            snap(&d, &w).microphone.as_deref(),
            Some("Fake Mic"),
            "resume"
        );
        d.discard(&p).unwrap();
        assert_eq!(snap(&d, &w).microphone, None, "discard");
        let (mut finished, _) = at(&w, Phase::Finished);
        assert!(
            snap(&finished, &w).microphone.is_some(),
            "kept when finished"
        );
        finished.new_session(&p).unwrap();
        assert_eq!(snap(&finished, &w).microphone, None, "new");
    }

    #[test]
    fn each_segment_is_resampled_on_its_own_whatever_its_device_format() {
        let w = World::default();
        {
            let mut captures = w.recorder.captures.borrow_mut();
            captures.push_back(Ok(Captured::new(vec![0.5; 96_000], 2, 48_000)));
            captures.push_back(Ok(Captured::new(vec![0.5; 16_000], 1, 16_000)));
        }
        let p = w.ports();
        let (mut d, mut jobs) = at(&w, Phase::Paused);
        save_input(&w, "AirPods Pro");
        d.resume(&p).unwrap();
        jobs.extend(d.pause(&p).unwrap());
        assert_eq!(snap(&d, &w).recorded_ms, 2_000, "two seconds");
        for index in [0, 1] {
            let samples = decode_wav(&w.files.segment(index).unwrap()).unwrap();
            assert_eq!(samples.len(), 16_000, "segment {index}");
        }
        for job in &jobs {
            finish(&mut d, &w, job);
        }
        let counts: Vec<usize> = w.transcriber.calls.borrow().iter().map(|c| c.0).collect();
        assert_eq!(counts, [16_000, 16_000], "each at 16 kHz mono");
    }

    #[test]
    fn a_device_that_answers_another_name_is_logged_as_a_mismatch() {
        let w = World::default();
        save_input(&w, "AirPods Pro");
        *w.recorder.answers_as.borrow_mut() = Some("Other Mic".to_string());
        let (d, _) = at(&w, Phase::Recording);
        assert_eq!(snap(&d, &w).phase, Phase::Recording, "carries on");
        assert_eq!(
            log_lines(&w),
            [
                r#"{"event":"start","started":null,"action":"record","wanted":"AirPods Pro","opened":"Other Mic","outcome":"opened","first_error":null,"second_error":null,"mismatch":true,"split_ms":0}"#
            ],
            "mismatch"
        );
    }

    #[test]
    fn each_start_outcome_is_logged_exactly() {
        let w = World::default();
        let _session = at(&w, Phase::Recording);
        assert_eq!(
            w.files.read(MICROPHONE_LOG).unwrap(),
            br#"{"event":"start","started":null,"action":"record","wanted":"","opened":"Fake Mic","outcome":"opened","first_error":null,"second_error":null,"mismatch":false,"split_ms":0}
"#,
            "opened"
        );

        let w2 = World::default();
        save_input(&w2, "Jabra");
        w2.recorder
            .unavailable
            .borrow_mut()
            .push("Jabra".to_string());
        let _session2 = at(&w2, Phase::Recording);
        assert_eq!(
            log_lines(&w2),
            [
                r#"{"event":"start","started":null,"action":"record","wanted":"Jabra","opened":"Fake Mic","outcome":"fallback","first_error":"gone","second_error":null,"mismatch":false,"split_ms":0}"#
            ],
            "fallback"
        );

        let w3 = World::default();
        save_input(&w3, "Jabra");
        w3.recorder
            .unavailable
            .borrow_mut()
            .push("Jabra".to_string());
        *w3.recorder.start_error.borrow_mut() = Some("no mic".to_string());
        let (mut d3, _) = at(&w3, Phase::Ready);
        assert!(d3.record(&w3.ports()).is_err(), "refused");
        assert_eq!(
            log_lines(&w3),
            [
                r#"{"event":"start","started":null,"action":"record","wanted":"Jabra","opened":null,"outcome":"failed","first_error":"gone","second_error":"no mic","mismatch":false,"split_ms":0}"#
            ],
            "failed after a fallback"
        );

        let w4 = World::default();
        *w4.recorder.start_error.borrow_mut() = Some("no mic".to_string());
        let (mut d4, _) = at(&w4, Phase::Ready);
        assert!(d4.record(&w4.ports()).is_err(), "refused");
        assert_eq!(
            log_lines(&w4),
            [
                r#"{"event":"start","started":null,"action":"record","wanted":"","opened":null,"outcome":"failed","first_error":"no mic","second_error":null,"mismatch":false,"split_ms":0}"#
            ],
            "failed"
        );
    }

    #[test]
    fn each_segment_is_logged_with_its_device_and_format() {
        let w = World::default();
        w.recorder
            .captures
            .borrow_mut()
            .push_back(Ok(Captured::new(vec![0.5, -0.75], 1, 16_000)));
        let _session = at(&w, Phase::Paused);
        assert_eq!(
            log_lines(&w).get(1).map(String::as_str),
            Some(
                r#"{"event":"segment","started":null,"device":"Fake Mic","channels":1,"sample_rate":16000,"samples":2,"peak":0.75,"cause":"stop","longest_silence_ms":0,"quietest_rms":0.0}"#
            ),
            "segment line"
        );

        let w2 = World::default();
        w2.recorder
            .captures
            .borrow_mut()
            .push_back(Ok(Captured::new(vec![0.0; 4], 2, 48_000)));
        let _session2 = at(&w2, Phase::Paused);
        assert_eq!(
            log_lines(&w2).get(1).map(String::as_str),
            Some(
                r#"{"event":"segment","started":null,"device":"Fake Mic","channels":2,"sample_rate":48000,"samples":4,"peak":0.0,"cause":"stop","longest_silence_ms":0,"quietest_rms":0.0}"#
            ),
            "silence"
        );
    }

    #[test]
    fn with_the_library_on_the_log_carries_the_entry_title() {
        let w = World::default();
        let p = w.library_ports();
        let mut d = idle(&w);
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let _job = d.pause(&p).unwrap();
        d.resume(&p).unwrap();
        let lines = log_lines(&w);
        assert!(
            lines
                .first()
                .is_some_and(|l| l.contains(r#""started":null,"action":"record""#)),
            "the first record has no start yet: {lines:?}"
        );
        assert!(
            lines.get(1).is_some_and(
                |l| l.starts_with(r#"{"event":"segment","started":"2026-10-26 14:03:09","#)
            ),
            "the segment carries the title: {lines:?}"
        );
        assert!(
            lines
                .get(2)
                .is_some_and(|l| l.contains(r#""started":"2026-10-26 14:03:09","action":"resume""#)),
            "resume: {lines:?}"
        );
    }

    #[test]
    fn a_failed_stop_logs_no_segment() {
        let w = World::default();
        w.recorder
            .captures
            .borrow_mut()
            .push_back(Err("unplugged".to_string()));
        let (mut d, _) = at(&w, Phase::Recording);
        assert!(d.pause(&w.ports()).unwrap().is_none(), "lost");
        assert_eq!(
            log_lines(&w).len(),
            1,
            "only the start: {:?}",
            log_lines(&w)
        );
    }

    #[test]
    fn the_log_keeps_the_newest_two_hundred_lines() {
        let w = World::default();
        *w.recorder.start_error.borrow_mut() = Some("no mic".to_string());
        let (mut d, _) = at(&w, Phase::Ready);
        save_input(&w, "first");
        assert!(d.record(&w.ports()).is_err(), "refused");
        save_input(&w, "");
        for _ in 0..200 {
            assert!(d.record(&w.ports()).is_err(), "refused");
        }
        let lines = log_lines(&w);
        assert_eq!(lines.len(), 200, "bounded");
        assert!(
            lines.iter().all(|l| !l.contains(r#""wanted":"first""#)),
            "the oldest is gone"
        );
        assert!(
            w.files.read(MICROPHONE_LOG).unwrap().ends_with(b"}\n"),
            "ends with a newline"
        );
    }

    #[test]
    fn an_unreadable_or_damaged_log_is_started_afresh() {
        let w = World::default();
        w.files.write(MICROPHONE_LOG, &[0xff, b'\n']).unwrap();
        let _session = at(&w, Phase::Recording);
        let lines = log_lines_lossy(&w);
        assert_eq!(lines.len(), 2, "kept lossily: {lines:?}");
        assert_eq!(lines.first().map(String::as_str), Some("\u{fffd}"), "lossy");
    }

    fn log_lines_lossy(w: &World) -> Vec<String> {
        String::from_utf8_lossy(&w.files.read(MICROPHONE_LOG).unwrap())
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_log_that_cannot_be_written_or_read_never_stops_a_recording() {
        let w = World::default();
        let (mut d, _) = at(&w, Phase::Ready);
        w.files.fail_write.set(true);
        d.record(&w.ports()).unwrap();
        assert_eq!(snap(&d, &w).phase, Phase::Recording, "write failed");

        let w2 = World::default();
        let (mut d2, _) = at(&w2, Phase::Ready);
        w2.files.fail_read.set(true);
        d2.record(&w2.ports()).unwrap();
        assert_eq!(snap(&d2, &w2).phase, Phase::Recording, "read failed");
    }

    #[test]
    fn new_leaves_the_log_in_place() {
        let w = World::default();
        let (mut d, _) = at(&w, Phase::Finished);
        let before = log_lines(&w);
        assert_eq!(before.len(), 2, "a start and a segment");
        d.new_session(&w.ports()).unwrap();
        assert_eq!(log_lines(&w), before, "untouched");
    }

    const TIGHTBEAM_HEARD: &str = "Open TightBeam now.";

    const TIGHTBEAM_FIXED: &str = "Open tightbeam now.";

    fn learned_file(w: &World) -> Option<Vec<u8>> {
        w.files.files.borrow().get(LEARNED).cloned()
    }

    fn learning_log(w: &World) -> Vec<Value> {
        w.files
            .files
            .borrow()
            .get(LEARNING_LOG)
            .map(|b| {
                String::from_utf8(b.clone())
                    .unwrap()
                    .lines()
                    .map(|l| serde_json::from_str(l).unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn learned_terms_come_first_in_the_prompt_and_the_cleanup_glossary() {
        let w = World::default();
        w.files.write(LEARNED, b"tightbeam\n").unwrap();
        w.files.set_vocabulary(Some("Rust\nTightBeam\n"));
        let (_, job) = to_cleanup(&w, CLAWED);
        let calls = w.transcriber.calls.borrow().clone();
        let (_, prompt) = calls.first().unwrap();
        assert!(
            prompt
                .as_deref()
                .unwrap()
                .ends_with("Glossary: tightbeam, Rust."),
            "{prompt:?}"
        );
        let _outcome = w.run_with(&job, &w.library_cleaner_ports());
        let requests = w.cleaner.calls.borrow();
        let (_, request) = requests.first().unwrap();
        assert!(
            request.starts_with("Glossary:\ntightbeam\nRust\n\nTranscript:\n"),
            "{request:?}"
        );
    }

    #[test]
    fn a_recovered_session_prompts_with_learned_terms_first() {
        let w = World::default();
        w.files.write(LEARNED, b"tightbeam\n").unwrap();
        w.files.set_vocabulary(Some("Rust\n"));
        w.files.put_segment(0, encode_wav(&speech(16_000)));
        let p = w.ports();
        let mut d = Dictation::recover(&p);
        let jobs = d.retry(&p).unwrap();
        let _: Snapshot = d.complete(w.run(jobs.first().unwrap()), &p);
        let calls = w.transcriber.calls.borrow();
        assert_eq!(
            calls.first().and_then(|(_, prompt)| prompt.clone()),
            Some(format!("{STYLE} Glossary: tightbeam, Rust.")),
            "prompt"
        );
    }

    #[test]
    fn learned_spellings_are_enforced_in_the_finished_transcript() {
        let w = World::default();
        w.files.write(LEARNED, b"tightbeam\n").unwrap();
        let (_, s) = heard_with(&w, &w.ports(), TIGHTBEAM_HEARD);
        assert_eq!(s.text, TIGHTBEAM_FIXED, "snapshot");
        assert_eq!(*w.clipboard.writes.borrow(), [TIGHTBEAM_FIXED], "clipboard");
    }

    #[test]
    fn the_raw_transcript_keeps_what_whisper_heard() {
        let w = World::default();
        w.files.write(LEARNED, b"tightbeam\n").unwrap();
        let (_, s) = heard_with(&w, &w.library_ports(), TIGHTBEAM_HEARD);
        assert_eq!(s.text, TIGHTBEAM_FIXED, "snapshot");
        assert_eq!(
            entry_file(&w, "transcript.txt").as_deref(),
            Some(TIGHTBEAM_FIXED),
            "entry"
        );
        assert_eq!(
            entry_file(&w, "transcript.raw.txt").as_deref(),
            Some(TIGHTBEAM_HEARD),
            "as heard"
        );
    }

    #[test]
    fn learned_spellings_are_enforced_after_cleanup() {
        let w = World::default();
        w.files.write(LEARNED, b"tightbeam\n").unwrap();
        let (mut d, job) = to_cleanup(&w, "open TightBeam now");
        let s = cleaned(&w, &mut d, &job, Ok(TIGHTBEAM_HEARD));
        assert_eq!(s.text, TIGHTBEAM_FIXED, "snapshot");
        assert_eq!(*w.clipboard.writes.borrow(), [TIGHTBEAM_FIXED], "clipboard");
        assert_eq!(
            entry_file(&w, "transcript.raw.txt").as_deref(),
            Some("open TightBeam now"),
            "as heard"
        );
    }

    #[test]
    fn vocabulary_terms_are_never_enforced() {
        let w = World::default();
        w.files.set_vocabulary(Some("TightBeam\n"));
        let (_, s) = heard_with(&w, &w.ports(), TIGHTBEAM_FIXED);
        assert_eq!(s.text, TIGHTBEAM_FIXED, "unchanged");
    }

    #[test]
    fn a_correction_copied_is_learned_once_and_logged() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let s = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        assert_eq!(s.learned, strings(&["tightbeam"]), "learned");
        assert_eq!(s.notice.as_deref(), Some(COPIED), "notice");
        assert_eq!(learned_file(&w).unwrap(), b"tightbeam\n", "file");
        assert_eq!(
            learning_log(&w),
            [json!({
                "event": "learn",
                "started": null,
                "found": ["tightbeam"],
                "added": ["tightbeam"],
                "outcome": "saved",
                "error": null,
            })],
            "log"
        );
        let again = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        assert_eq!(again.learned, strings(&["tightbeam"]), "still shown");
        assert_eq!(learning_log(&w).len(), 1, "no new log line");
        let fresh = d.apply(Action::New, "", &p).unwrap();
        assert!(fresh.learned.is_empty(), "New forgets it");
        assert_eq!(learned_file(&w).unwrap(), b"tightbeam\n", "the file stays");
    }

    #[test]
    fn discard_forgets_what_was_learned() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let _: Snapshot = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        let s = d.apply(Action::Discard, "", &p).unwrap();
        assert!(s.learned.is_empty(), "forgotten");
    }

    #[test]
    fn the_learning_log_names_the_session() {
        let w = World::default();
        let p = w.library_ports();
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(Ok(TIGHTBEAM_HEARD.to_string()));
        let mut d = idle(&w);
        let s = library_session(&w, &mut d, 1);
        assert_eq!(s.text, TIGHTBEAM_HEARD, "heard");
        let _: Snapshot = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        let log = learning_log(&w);
        assert_eq!(
            log.first().and_then(|l| l.get("started")),
            Some(&json!("2026-10-26 14:03:09")),
            "{log:?}"
        );
    }

    #[test]
    fn a_copy_the_clipboard_refuses_learns_nothing() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        w.clipboard.fail.set(true);
        assert!(
            matches!(
                d.apply(Action::Copy, TIGHTBEAM_FIXED, &p),
                Err(DictationError::Clipboard(_))
            ),
            "refused"
        );
        assert_eq!(learned_file(&w), None, "no file");
        assert!(learning_log(&w).is_empty(), "no log");
    }

    #[test]
    fn an_insertion_learns_nothing_and_logs_nothing() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let s = d
            .apply(Action::Copy, "Open TightBeam right now.", &p)
            .unwrap();
        assert!(s.learned.is_empty(), "nothing learned");
        assert_eq!(learned_file(&w), None, "no file");
        assert!(learning_log(&w).is_empty(), "no log");
    }

    #[test]
    fn a_damaged_learned_list_is_left_alone() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        w.files.write(LEARNED, &[0xff]).unwrap();
        let s = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        assert!(s.learned.is_empty(), "nothing learned");
        assert_eq!(learned_file(&w).unwrap(), [0xff], "untouched");
        assert_eq!(
            w.clipboard.writes.borrow().last().map(String::as_str),
            Some(TIGHTBEAM_FIXED),
            "copied"
        );
        let log = learning_log(&w);
        assert_eq!(
            log.first().and_then(|l| l.get("outcome")),
            Some(&json!("unreadable")),
            "{log:?}"
        );
    }

    #[test]
    fn a_learned_list_that_cannot_be_written_is_logged() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        *w.files.fail_write_suffix.borrow_mut() = Some("learned.txt.partial".to_string());
        let s = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        assert!(s.learned.is_empty(), "nothing learned");
        assert_eq!(learned_file(&w), None, "no file");
        assert_eq!(
            w.clipboard.writes.borrow().last().map(String::as_str),
            Some(TIGHTBEAM_FIXED),
            "copied"
        );
        let log = learning_log(&w);
        let line = log.first().unwrap();
        assert_eq!(line.get("outcome"), Some(&json!("write_failed")), "{log:?}");
        assert_eq!(line.get("error"), Some(&json!("disk full")), "{log:?}");
        assert_eq!(line.get("added"), Some(&json!([])), "{log:?}");
    }

    #[test]
    fn a_rewrite_is_capped_and_logged() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(
            &w,
            &p,
            "Use TightBeam and Scribe and Github and Svelt and Ollama and Cargo now.",
        );
        let s = d
            .apply(
                Action::Copy,
                "Use tightbeam and Scriber and GitHub and Svelte and ollama and cargo now.",
                &p,
            )
            .unwrap();
        assert!(s.learned.is_empty(), "nothing learned");
        assert_eq!(learned_file(&w), None, "no file");
        let log = learning_log(&w);
        let line = log.first().unwrap();
        assert_eq!(line.get("outcome"), Some(&json!("capped")), "{log:?}");
        assert_eq!(
            line.get("found").and_then(Value::as_array).map(Vec::len),
            Some(6),
            "{log:?}"
        );
    }

    #[test]
    fn a_term_already_in_the_vocabulary_is_not_added() {
        let w = World::default();
        w.files.set_vocabulary(Some("tightbeam\n"));
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let s = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        assert!(s.learned.is_empty(), "nothing learned");
        assert_eq!(learned_file(&w), None, "no file");
        let log = learning_log(&w);
        assert_eq!(
            log.first().and_then(|l| l.get("outcome")),
            Some(&json!("nothing_added")),
            "{log:?}"
        );
    }

    #[test]
    fn what_was_learned_is_forgotten_at_relaunch_but_kept_on_disk() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let _: Snapshot = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        assert!(
            Dictation::recover(&p).snapshot(&p).learned.is_empty(),
            "a relaunch shows nothing learned"
        );
        assert_eq!(learned_file(&w).unwrap(), b"tightbeam\n", "kept");
    }

    #[test]
    fn undo_restores_the_learned_list() {
        let w = World::default();
        w.files.write(LEARNED, b"Scriber\nTightBeam\n").unwrap();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        // TightBeam is already learned, so the transcript shows it as is.
        let _: Snapshot = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        assert_eq!(
            learned_file(&w).unwrap(),
            b"tightbeam\nScriber\n",
            "learned"
        );
        let s = d.apply(Action::UndoLearn, "", &p).unwrap();
        assert_eq!(
            learned_file(&w).unwrap(),
            b"Scriber\nTightBeam\n",
            "restored"
        );
        assert!(s.learned.is_empty(), "forgotten");
        assert_eq!(s.notice.as_deref(), Some(COPIED), "notice unchanged");
        assert_eq!(
            learning_log(&w).last(),
            Some(&json!({
                "event": "undo",
                "started": null,
                "removed": ["tightbeam"],
                "outcome": "restored",
                "error": null,
            })),
            "log"
        );
    }

    #[test]
    fn undo_of_the_first_term_leaves_an_empty_list() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let _: Snapshot = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        let _: Snapshot = d.apply(Action::UndoLearn, "", &p).unwrap();
        assert_eq!(learned_file(&w).unwrap(), b"", "0 bytes");
    }

    #[test]
    fn an_undo_that_cannot_be_written_changes_nothing_and_can_be_tried_again() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let _: Snapshot = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        let before = snap(&d, &w);
        *w.files.fail_write_suffix.borrow_mut() = Some("learned.txt.partial".to_string());
        let refused = d.apply(Action::UndoLearn, "", &p).unwrap_err();
        assert_eq!(
            refused.to_string(),
            "Could not undo learning: disk full",
            "error"
        );
        assert_eq!(snap(&d, &w), before, "unchanged");
        assert_eq!(learned_file(&w).unwrap(), b"tightbeam\n", "file unchanged");
        assert_eq!(
            learning_log(&w)
                .last()
                .and_then(|l| l.get("outcome"))
                .cloned(),
            Some(json!("write_failed")),
            "logged"
        );
        *w.files.fail_write_suffix.borrow_mut() = None;
        let undone = d.apply(Action::UndoLearn, "", &p).unwrap();
        assert!(undone.learned.is_empty(), "undone");
        assert_eq!(learned_file(&w).unwrap(), b"", "restored");
    }

    #[test]
    fn undo_with_nothing_learned_only_bumps_the_revision() {
        let w = World::default();
        w.files.write(LEARNED, b"tightbeam\n").unwrap();
        let p = w.ports();
        let (mut d, before) = heard_with(&w, &p, "Hello.");
        let s = d.apply(Action::UndoLearn, "", &p).unwrap();
        assert_eq!(s.revision, before.revision.wrapping_add(1), "revision");
        assert_eq!(
            Snapshot {
                revision: before.revision,
                ..s
            },
            before,
            "nothing else"
        );
        assert_eq!(learned_file(&w).unwrap(), b"tightbeam\n", "file unchanged");
        assert!(learning_log(&w).is_empty(), "no log");
    }

    /// A recording of two segments: job 0 and job 1, both pending, session paused.
    fn two_parts(w: &World) -> (Dictation, Job, Job) {
        let p = w.ports();
        let mut d = idle(w);
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let first = d.pause(&p).unwrap().unwrap();
        d.resume(&p).unwrap();
        let second = d.pause(&p).unwrap().unwrap();
        (d, first, second)
    }

    fn answer(w: &World, d: &mut Dictation, job: &Job, reply: Result<&str, &str>) -> Snapshot {
        w.transcriber
            .answers
            .borrow_mut()
            .push_back(reply.map(str::to_string).map_err(str::to_string));
        d.complete(w.run(job), &w.ports())
    }

    /// The fields the transcript-so-far feature publishes.
    fn views(snapshot: &Snapshot) -> (&str, &str, Option<&str>) {
        (
            snapshot.partial.as_str(),
            snapshot.text.as_str(),
            snapshot.raw_text.as_deref(),
        )
    }

    #[test]
    fn partial_grows_as_segments_land() {
        let w = World::default();
        let p = w.ports();
        let mut d = idle(&w);
        d.new_session(&p).unwrap();
        d.record(&p).unwrap();
        let first = d.pause(&p).unwrap().unwrap();
        assert_eq!(snap(&d, &w).partial, "", "before the outcome");
        d.resume(&p).unwrap();
        let one = answer(&w, &mut d, &first, Ok("One."));
        assert_eq!(one.phase, Phase::Recording, "phase");
        assert_eq!(views(&one), ("One.", "", None), "first");
        let second = d.pause(&p).unwrap().unwrap();
        let two = answer(&w, &mut d, &second, Ok("Two."));
        assert_eq!(two.phase, Phase::Paused, "phase");
        assert_eq!(views(&two), ("One.\n\nTwo.", "", None), "second");
    }

    #[test]
    fn partial_is_in_recording_order_not_arrival_order() {
        let w = World::default();
        let (mut d, first, second) = two_parts(&w);
        let late = answer(&w, &mut d, &second, Ok("Two."));
        assert_eq!(late.partial, "Two.", "second alone");
        let both = answer(&w, &mut d, &first, Ok("One."));
        assert_eq!(both.partial, "One.\n\nTwo.", "ordered");
    }

    #[test]
    fn partial_skips_failed_segments() {
        let w = World::default();
        let (mut d, first, second) = two_parts(&w);
        let _: Snapshot = answer(&w, &mut d, &first, Ok("One."));
        let failed = answer(&w, &mut d, &second, Err("boom"));
        assert_eq!(
            (failed.phase, failed.partial.as_str()),
            (Phase::Paused, "One.")
        );
        assert!(d.done(&w.ports()).unwrap().is_none(), "paused");
        let after = snap(&d, &w);
        assert_eq!(
            (after.phase, after.partial.as_str(), after.raw_text),
            (Phase::Failed, "One.", None),
            "failed"
        );
    }

    #[test]
    fn partial_is_published_while_transcribing() {
        let w = World::default();
        let (mut d, first, _second) = two_parts(&w);
        assert!(d.done(&w.ports()).unwrap().is_none(), "paused");
        let got = answer(&w, &mut d, &first, Ok("One."));
        assert_eq!(
            (got.phase, got.partial.as_str()),
            (Phase::Transcribing, "One.")
        );
    }

    #[test]
    fn raw_text_is_whispers_text_without_the_library() {
        let w = World::default();
        assert!(w.ports().library.is_none(), "library off");
        let (_, got) = heard_with(&w, &w.ports(), SHORTCUT);
        assert_eq!(got.text, SHORTCUT_GLYPHS, "text");
        assert_eq!(got.raw_text.as_deref(), Some(SHORTCUT), "raw");
        assert_eq!(got.partial, SHORTCUT, "partial");
    }

    #[test]
    fn raw_text_is_what_the_library_archives() {
        let w = World::default();
        let (_, got) = heard_with(&w, &w.library_ports(), SHORTCUT);
        assert_eq!(got.raw_text.as_deref(), Some(SHORTCUT), "raw");
        assert_eq!(
            entry_file(&w, "transcript.raw.txt").as_deref(),
            Some(SHORTCUT),
            "archive"
        );
    }

    #[test]
    fn raw_text_is_published_even_when_equal_to_text() {
        let w = World::default();
        let (_, got) = heard_with(&w, &w.ports(), "Hello.");
        assert_eq!(got.text, "Hello.", "text");
        assert_eq!(got.raw_text.as_deref(), Some("Hello."), "raw");
    }

    #[test]
    fn raw_text_is_whispers_text_before_cleanup() {
        let w = World::default();
        let (mut d, job) = to_cleanup(&w, CLAWED);
        let got = cleaned(&w, &mut d, &job, Ok(CLAUDE));
        assert_eq!(got.text, CLAUDE, "text");
        assert_eq!(got.raw_text.as_deref(), Some(CLAWED), "raw");
    }

    #[test]
    fn raw_text_is_whispers_text_when_cleanup_fails() {
        let w = World::default();
        let (mut d, job) = to_cleanup(&w, "Hello.");
        let got = cleaned(&w, &mut d, &job, Err("down"));
        assert_eq!(got.raw_text.as_deref(), Some("Hello."), "cleanup failed");
    }

    #[test]
    fn raw_text_is_before_learned_spellings() {
        let w = World::default();
        w.files.write(LEARNED, b"tightbeam\n").unwrap();
        let (_, got) = heard_with(&w, &w.ports(), TIGHTBEAM_HEARD);
        assert_eq!(got.text, TIGHTBEAM_FIXED, "text");
        assert_eq!(got.raw_text.as_deref(), Some(TIGHTBEAM_HEARD), "raw");
    }

    #[test]
    fn raw_text_survives_copy() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, SHORTCUT);
        let edited = d.apply(Action::Copy, "Edited.", &p).unwrap();
        assert_eq!(edited.raw_text.as_deref(), Some(SHORTCUT), "edited");
        let same = d.apply(Action::Copy, SHORTCUT, &p).unwrap();
        assert_eq!(same.raw_text.as_deref(), Some(SHORTCUT), "same");
    }

    #[test]
    fn new_and_discard_clear_partial_and_raw_text() {
        for action in [Action::New, Action::Discard] {
            let w = World::default();
            let p = w.ports();
            let (mut d, before) = heard_with(&w, &p, SHORTCUT);
            assert!(before.raw_text.is_some(), "finished");
            let after = d.apply(action, "", &p).unwrap();
            assert_eq!(
                (after.partial.as_str(), after.raw_text),
                ("", None),
                "{action:?}"
            );
        }
    }

    #[test]
    fn a_recovered_session_with_segments_has_no_partial_or_raw_text() {
        let w = World::default();
        w.files.put_segment(0, encode_wav(&speech(16_000)));
        let got = snap(&Dictation::recover(&w.ports()), &w);
        assert_eq!(
            (got.phase, got.partial.as_str(), got.raw_text),
            (Phase::Failed, "", None)
        );
    }

    #[test]
    fn a_recovered_kept_transcript_has_no_partial_or_raw_text() {
        let w = World::default();
        let _: (Dictation, Snapshot) = heard_with(&w, &w.ports(), "Kept.");
        let got = snap(&Dictation::recover(&w.ports()), &w);
        assert_eq!(
            (got.phase, got.partial.as_str(), got.raw_text),
            (Phase::Finished, "", None)
        );
    }

    #[test]
    fn the_last_partial_is_the_raw_text() {
        let w = World::default();
        let (mut d, first, second) = two_parts(&w);
        let _: Snapshot = answer(&w, &mut d, &first, Ok("One."));
        let last = answer(&w, &mut d, &second, Ok("Two."));
        assert!(d.done(&w.ports()).unwrap().is_none(), "paused");
        let done = snap(&d, &w);
        assert_eq!(done.phase, Phase::Finished, "settled");
        assert_eq!(done.raw_text.as_deref(), Some(last.partial.as_str()));
    }

    #[test]
    fn accept_learns_from_a_correction_without_touching_the_clipboard() {
        let w = World::default();
        let p = w.ports();
        let (mut d, before) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let writes = w.clipboard.writes.borrow().len();
        let got = d.apply(Action::Accept, TIGHTBEAM_FIXED, &p).unwrap();
        assert_eq!(got.learned, ["tightbeam"], "learned");
        assert_eq!(learned_file(&w).unwrap(), b"tightbeam\n", "file");
        assert_eq!(got.text, TIGHTBEAM_FIXED, "text");
        assert_eq!(w.clipboard.writes.borrow().len(), writes, "no write");
        assert_eq!(got.notice, before.notice, "no notice change");
        let line = learning_log(&w).pop().unwrap();
        assert_eq!(line["event"], "learn");
        assert_eq!(line["outcome"], "saved");
        assert_eq!(line["added"], json!(["tightbeam"]));
    }

    #[test]
    fn accept_logs_what_copy_logs() {
        let by_accept = World::default();
        let (mut d, _) = heard_with(&by_accept, &by_accept.ports(), TIGHTBEAM_HEARD);
        let _: Snapshot = d
            .apply(Action::Accept, TIGHTBEAM_FIXED, &by_accept.ports())
            .unwrap();
        let by_copy = World::default();
        let (mut c, _) = heard_with(&by_copy, &by_copy.ports(), TIGHTBEAM_HEARD);
        let _: Snapshot = c
            .apply(Action::Copy, TIGHTBEAM_FIXED, &by_copy.ports())
            .unwrap();
        assert_eq!(learning_log(&by_accept), learning_log(&by_copy));
    }

    #[test]
    fn accept_with_no_correction_learns_nothing() {
        let w = World::default();
        let p = w.ports();
        let (mut d, before) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let writes = w.clipboard.writes.borrow().len();
        let file = learned_file(&w);
        let got = d.apply(Action::Accept, TIGHTBEAM_HEARD, &p).unwrap();
        assert!(got.learned.is_empty(), "learned");
        assert_eq!(learned_file(&w), file, "file");
        assert_eq!(w.clipboard.writes.borrow().len(), writes, "clipboard");
        assert_eq!(got.notice, before.notice, "notice");
    }

    #[test]
    fn accept_is_refused_outside_finished() {
        for phase in PHASES {
            if phase == Phase::Finished {
                continue;
            }
            let w = World::default();
            let (mut d, _jobs) = at(&w, phase);
            let refused = d.apply(Action::Accept, "x", &w.ports()).unwrap_err();
            assert_eq!(refused.to_string(), format!("Cannot accept while {phase}."));
            assert!(w.clipboard.writes.borrow().is_empty(), "{phase}");
        }
    }

    #[test]
    fn undo_after_accept_restores_the_learned_list() {
        let w = World::default();
        let p = w.ports();
        w.files.write(LEARNED, b"Scriber\nTightBeam\n").unwrap();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let _: Snapshot = d.apply(Action::Accept, TIGHTBEAM_FIXED, &p).unwrap();
        assert_eq!(learned_file(&w).unwrap(), b"tightbeam\nScriber\n");
        let undone = d.apply(Action::UndoLearn, "", &p).unwrap();
        assert_eq!(learned_file(&w).unwrap(), b"Scriber\nTightBeam\n");
        assert!(undone.learned.is_empty(), "learned");
    }

    #[test]
    fn undo_after_accept_can_empty_the_learned_list() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let _: Snapshot = d.apply(Action::Accept, TIGHTBEAM_FIXED, &p).unwrap();
        let _: Snapshot = d.apply(Action::UndoLearn, "", &p).unwrap();
        assert_eq!(learned_file(&w).unwrap(), b"", "emptied");
    }

    #[test]
    fn copy_still_writes_the_clipboard_then_learns() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        w.clipboard.fail.set(true);
        let refused = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap_err();
        assert!(
            matches!(refused, DictationError::Clipboard(_)),
            "{refused:?}"
        );
        assert!(learned_file(&w).is_none(), "nothing learned");
        w.clipboard.fail.set(false);
        let copied = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        assert_eq!(copied.learned, ["tightbeam"], "learned");
        assert_eq!(copied.notice.as_deref(), Some(COPIED), "notice");
        assert_eq!(
            w.clipboard.writes.borrow().last().map(String::as_str),
            Some(TIGHTBEAM_FIXED),
            "clipboard"
        );
    }

    #[test]
    fn accept_then_copy_of_the_same_text_learns_nothing_new() {
        let w = World::default();
        let p = w.ports();
        let (mut d, _) = heard_with(&w, &p, TIGHTBEAM_HEARD);
        let _: Snapshot = d.apply(Action::Accept, TIGHTBEAM_FIXED, &p).unwrap();
        let copied = d.apply(Action::Copy, TIGHTBEAM_FIXED, &p).unwrap();
        assert_eq!(copied.learned, ["tightbeam"], "kept");
        assert_eq!(learned_file(&w).unwrap(), b"tightbeam\n", "file");
    }

    // The silence split: segments the recorder closed at a silence.

    fn save_settings(w: &World, settings: &Value) {
        w.files
            .write(SETTINGS, &serde_json::to_vec(settings).unwrap())
            .unwrap();
    }

    fn splits(w: &World) -> Vec<u64> {
        w.recorder.splits.borrow().clone()
    }

    /// Whether `job` is the segment at `index`.
    fn is_index(job: &Job, index: u32) -> bool {
        format!("{job:?}").contains(&format!("index: {index},"))
    }

    /// A recording session with the split at 1.5 s.
    fn splitting(w: &World) -> Dictation {
        save_settings(w, &json!({ "silence_split_ms": 1_500 }));
        let (d, _) = at(w, Phase::Recording);
        d
    }

    #[test]
    fn record_and_resume_pass_the_saved_split() {
        let w = World::default();
        let (mut d, _) = at(&w, Phase::Paused);
        d.resume(&w.ports()).unwrap();
        assert_eq!(splits(&w), [0, 0], "no settings");
        assert!(
            log_lines(&w)[0].ends_with(r#","mismatch":false,"split_ms":0}"#),
            "logged off"
        );

        let w2 = World::default();
        save_settings(&w2, &json!({ "silence_split_ms": 2_500 }));
        let (mut d2, _) = at(&w2, Phase::Paused);
        d2.resume(&w2.ports()).unwrap();
        assert_eq!(splits(&w2), [2_500, 2_500], "saved");
        let lines = log_lines(&w2);
        assert!(
            lines[0].ends_with(r#","split_ms":2500}"#)
                && lines[2].ends_with(r#","split_ms":2500}"#),
            "logged: {lines:?}"
        );

        let w3 = World::default();
        save_settings(
            &w3,
            &json!({ "input_device": "USB Mic", "silence_split_ms": 2_500 }),
        );
        w3.recorder
            .unavailable
            .borrow_mut()
            .push("USB Mic".to_string());
        let _session = at(&w3, Phase::Recording);
        assert_eq!(opened(&w3), ["USB Mic", ""], "fell back");
        assert_eq!(splits(&w3), [2_500, 2_500], "both attempts");
        assert!(
            log_lines(&w3)[0].ends_with(r#","split_ms":2500}"#),
            "fallback logged"
        );

        let w4 = World::default();
        let (mut d4, _) = at(&w4, Phase::Paused);
        save_settings(&w4, &json!({ "silence_split_ms": 2_500 }));
        d4.resume(&w4.ports()).unwrap();
        assert_eq!(splits(&w4), [0, 2_500], "read at each start");
    }

    #[test]
    fn a_split_closes_a_segment_while_recording_goes_on() {
        let w = World::default();
        let p = w.ports();
        let mut d = splitting(&w);
        let before = snap(&d, &w).revision;
        w.recorder.closed.borrow_mut().push_back(captured_speech(1));
        let s = d.split(&p).unwrap();
        assert_eq!(s, snap(&d, &w), "answers with the snapshot");
        assert_eq!(
            (s.phase, s.segments, s.pending, s.recorded_ms, s.revision),
            (Phase::Recording, 1, 1, 1_000, before + 1),
            "recording goes on"
        );
        assert_eq!(w.queue.jobs.borrow().len(), 1, "one job");
        let first = w.queue.jobs.borrow()[0].clone();
        assert!(is_index(&first, 0), "segment 0");
        let _outcome = w.run(&first);
        assert_eq!(
            w.transcriber.calls.borrow()[0].0,
            16_000,
            "the whole second"
        );
        assert_eq!(
            decode_wav(&w.files.segment(0).unwrap()).unwrap().len(),
            16_000,
            "kept for Retry"
        );
        assert_eq!(
            *w.recorder.calls.borrow(),
            ["start", "take_closed"],
            "no stop, no second start"
        );
        let lines = log_lines(&w);
        let last = lines.last().unwrap();
        assert!(
            last.starts_with(r#"{"event":"segment""#) && last.contains(r#""cause":"silence""#),
            "{last}"
        );

        let second = d.pause(&p).unwrap().unwrap();
        let _second_outcome = w.run(&second);
        let calls = w.transcriber.calls.borrow().clone();
        assert_eq!(calls[1].1, calls[0].1, "the same prompt");
        assert!(calls[0].1.is_some(), "a prompt");
        assert!(
            log_lines(&w).last().unwrap().contains(r#""cause":"stop""#),
            "pause"
        );
    }

    #[test]
    fn several_closed_segments_close_in_order_with_one_revision() {
        let w = World::default();
        let p = w.ports();
        let mut d = splitting(&w);
        let before = snap(&d, &w).revision;
        w.recorder
            .closed
            .borrow_mut()
            .extend([captured_speech(1), captured_speech(1)]);
        let s = d.split(&p).unwrap();
        assert_eq!(
            (s.segments, s.pending, s.recorded_ms, s.revision),
            (2, 2, 2_000, before + 1),
            "two closed"
        );
        let jobs = w.queue.jobs.borrow().clone();
        assert!(
            is_index(&jobs[0], 0) && is_index(&jobs[1], 1),
            "in order: {jobs:?}"
        );
    }

    #[test]
    fn nothing_closed_means_no_split() {
        let w = World::default();
        let mut d = splitting(&w);
        let before = snap(&d, &w);
        assert_eq!(d.split(&w.ports()), None, "nothing");
        assert_eq!(snap(&d, &w), before, "unchanged");
    }

    #[test]
    fn no_split_unless_recording() {
        for phase in [
            Phase::Ready,
            Phase::Paused,
            Phase::Transcribing,
            Phase::Finished,
        ] {
            let w = World::default();
            let (mut d, _) = at(&w, phase);
            w.recorder.closed.borrow_mut().push_back(captured_speech(1));
            assert_eq!(d.split(&w.ports()), None, "{phase:?}");
            assert!(
                !w.recorder.calls.borrow().contains(&"take_closed"),
                "{phase:?}"
            );
        }
    }

    #[test]
    fn a_split_then_pause_then_done_joins_both_parts() {
        let w = World::default();
        let p = w.ports();
        let mut d = splitting(&w);
        w.recorder.closed.borrow_mut().push_back(captured_speech(1));
        let _split = d.split(&p).unwrap();
        let second = d.pause(&p).unwrap().unwrap();
        assert!(is_index(&second, 1), "the next index");
        assert!(d.done(&p).unwrap().is_none(), "nothing was recording");
        let first = w.queue.jobs.borrow()[0].clone();
        let _two = answer(&w, &mut d, &second, Ok("Two."));
        let done = answer(&w, &mut d, &first, Ok("One."));
        assert_eq!(done.phase, Phase::Finished, "finished");
        assert_eq!(
            done.text,
            format!("One.{}Two.", scriber_core::transcript::SEGMENT_SEPARATOR),
            "joined in recording order"
        );
    }

    #[test]
    fn a_split_the_queue_refuses_fails_at_once() {
        let w = World::default();
        let mut d = splitting(&w);
        w.queue.fail.set(true);
        w.recorder.closed.borrow_mut().push_back(captured_speech(1));
        let s = d.split(&w.ports()).unwrap();
        assert_eq!(
            (s.phase, s.segments, s.pending),
            (Phase::Recording, 1, 0),
            "failed, not pending"
        );
        w.queue.fail.set(false);
        let done = d.apply(Action::Done, "", &w.ports()).unwrap();
        assert_eq!(
            (done.segments, done.pending),
            (2, 1),
            "the paused part queued"
        );
        let pause = w.queue.jobs.borrow()[0].clone();
        let failed = answer(&w, &mut d, &pause, Ok("Two."));
        assert_eq!(failed.phase, Phase::Failed, "the split part failed");
        assert!(
            failed
                .error
                .unwrap()
                .starts_with("Part 1 of 2 could not be"),
            "which"
        );
    }

    #[test]
    fn a_split_whose_audio_cannot_be_kept_still_transcribes() {
        let w = World::default();
        let mut d = splitting(&w);
        *w.files.fail_write_suffix.borrow_mut() = Some(".wav".to_string());
        w.recorder.closed.borrow_mut().push_back(captured_speech(1));
        let s = d.split(&w.ports()).unwrap();
        assert_eq!(
            s.notice.as_deref(),
            Some("Could not keep this part's audio for Retry: disk full."),
            "notice"
        );
        assert_eq!(w.queue.jobs.borrow().len(), 1, "still queued");
    }

    #[test]
    fn a_split_keeps_the_fallback_notice() {
        let w = World::default();
        save_settings(
            &w,
            &json!({ "input_device": "Jabra", "silence_split_ms": 1_500 }),
        );
        w.recorder
            .unavailable
            .borrow_mut()
            .push("Jabra".to_string());
        let (mut d, _) = at(&w, Phase::Recording);
        w.recorder.closed.borrow_mut().push_back(captured_speech(1));
        let s = d.split(&w.ports()).unwrap();
        assert_eq!(s.notice, Some(input_fallback("Jabra", "Fake Mic")), "kept");
    }

    #[test]
    fn a_split_part_fills_in_the_partial_while_recording() {
        let w = World::default();
        let p = w.ports();
        let mut d = splitting(&w);
        w.recorder.closed.borrow_mut().push_back(captured_speech(1));
        let _split = d.split(&p).unwrap();
        let first = w.queue.jobs.borrow()[0].clone();
        let s = answer(&w, &mut d, &first, Ok("One."));
        assert_eq!(
            (s.phase, s.partial.as_str()),
            (Phase::Recording, "One."),
            "partial"
        );
    }

    #[test]
    fn segment_lines_carry_the_silence_profile() {
        let w = World::default();
        let mut speech_then_silence = captured_speech(1);
        speech_then_silence.samples.extend(vec![0.0; 24_000]);
        w.recorder
            .captures
            .borrow_mut()
            .push_back(Ok(speech_then_silence));
        let _session = at(&w, Phase::Paused);
        assert!(
            log_lines(&w)[1]
                .ends_with(r#","cause":"stop","longest_silence_ms":1500,"quietest_rms":0.0}"#),
            "{:?}",
            log_lines(&w)
        );

        let w2 = World::default();
        w2.recorder
            .captures
            .borrow_mut()
            .push_back(Ok(Captured::new(vec![0.5; 16_000], 1, 16_000)));
        let _session2 = at(&w2, Phase::Paused);
        assert!(
            log_lines(&w2)[1]
                .ends_with(r#","cause":"stop","longest_silence_ms":0,"quietest_rms":0.5}"#),
            "{:?}",
            log_lines(&w2)
        );
    }

    #[test]
    fn the_recorder_takes_a_split_notifier() {
        use scriber_core::ports::Recorder as _;
        let w = World::default();
        w.recorder.on_split(std::sync::Arc::new(|| {}));
        assert_eq!(*w.recorder.calls.borrow(), ["on_split"], "kept");
    }
}
