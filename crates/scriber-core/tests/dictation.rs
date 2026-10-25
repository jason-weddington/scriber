//! The dictation session's state machine, against fake ports.

mod common;

#[cfg(test)]
mod tests {
    use scriber_core::audio::{decode_wav, encode_wav};
    use scriber_core::dictation::{
        COPIED, Dictation, DictationError, Job, NOTHING_HEARD, model_missing,
    };
    use scriber_core::model::{Action, Phase, Snapshot};
    use scriber_core::ports::{Captured, PortError};
    use scriber_core::vocabulary::STYLE;

    use crate::common::{World, speech};

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

    const ACTIONS: [Action; 8] = [
        Action::New,
        Action::Record,
        Action::Pause,
        Action::Resume,
        Action::Done,
        Action::Discard,
        Action::Retry,
        Action::Copy,
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
            Phase::Finished => matches!(action, A::New | A::Discard | A::Copy),
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
                "copy"
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
}
