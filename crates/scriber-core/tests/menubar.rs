//! The menu bar's pure decisions: the toggle table, the six items and their
//! flags, the shortcut grammar, the three icons and the busy icon's pulse
//! frames, the notification rule, the copy sound's rule and the log lines.

#[expect(dead_code, reason = "these tests need only some of the fakes")]
mod common;

#[cfg(test)]
mod tests {
    use scriber_core::dictation::{COPIED, Dictation, DictationError, NOTHING_HEARD};
    use scriber_core::menubar::{
        COPY_SOUND, DEFAULT_PAUSE_SHORTCUT, DEFAULT_SHORTCUT, Icon, Item, Log, PULSE_TICK_MS,
        Sound, Toggle, accelerator, accelerator_or_default, icon, icon_at, items, notice, pause,
        sound, toggle,
    };
    use scriber_core::model::{Action, Phase, Snapshot};

    use crate::common::World;

    const PHASES: [Phase; 7] = [
        Phase::Idle,
        Phase::Ready,
        Phase::Recording,
        Phase::Paused,
        Phase::Transcribing,
        Phase::Finished,
        Phase::Failed,
    ];

    /// The dictation tests' driver: a session driven to `phase`.
    fn session(w: &World, phase: Phase) -> Dictation {
        let p = w.ports();
        let mut d = Dictation::recover(&p);
        if phase == Phase::Idle {
            return d;
        }
        d.new_session(&p).unwrap();
        if phase == Phase::Ready {
            return d;
        }
        d.record(&p).unwrap();
        if phase == Phase::Recording {
            return d;
        }
        let job = d.pause(&p).unwrap().unwrap();
        if phase == Phase::Paused {
            return d;
        }
        if phase == Phase::Failed {
            w.transcriber
                .answers
                .borrow_mut()
                .push_back(Err("model exploded".to_string()));
        }
        assert!(d.done(&p).unwrap().is_none(), "nothing was recording");
        if phase == Phase::Transcribing {
            return d;
        }
        let snapshot = d.complete(w.run(&job), &p);
        assert_eq!(snapshot.phase, phase, "reached");
        d
    }

    /// The item with `id` in `phase`.
    fn item(phase: Phase, id: &str) -> Item {
        items(phase)
            .into_iter()
            .find(|item| item.id == id)
            .unwrap_or_else(|| panic!("no item {id}"))
    }

    /// A finished snapshot carrying `notice`.
    fn finished(notice: Option<&str>, revision: u64) -> Snapshot {
        Snapshot {
            revision,
            phase: Phase::Finished,
            recorded_ms: 1_000,
            segments: 1,
            pending: 0,
            text: "First thought.".to_string(),
            error: None,
            notice: notice.map(str::to_string),
            model_error: None,
            download: None,
            library_revision: 1,
            microphone: None,
            learned: vec![],
            partial: String::new(),
            raw_text: None,
            post_processing: false,
        }
    }

    #[test]
    fn the_toggle_table_is_pinned() {
        for phase in PHASES {
            let want = match phase {
                Phase::Idle | Phase::Ready | Phase::Finished => Toggle::Record,
                Phase::Recording => Toggle::Act(Action::Done),
                Phase::Paused => Toggle::Act(Action::Resume),
                Phase::Transcribing | Phase::Failed => Toggle::Nothing,
            };
            assert_eq!(toggle(phase), want, "{phase}");
        }
    }

    #[test]
    fn the_pause_table_is_pinned() {
        for phase in PHASES {
            let want = match phase {
                Phase::Recording => Toggle::Act(Action::Pause),
                Phase::Paused => Toggle::Act(Action::Resume),
                Phase::Idle
                | Phase::Ready
                | Phase::Transcribing
                | Phase::Finished
                | Phase::Failed => Toggle::Nothing,
            };
            assert_eq!(pause(phase), want, "{phase}");
            assert_ne!(pause(phase), Toggle::Record, "{phase}: never records");
        }
    }

    #[test]
    fn the_menu_is_six_items_with_show_and_quit_always_enabled() {
        for phase in PHASES {
            let [show, _, _, _, _, quit] = items(phase);
            assert_eq!(
                (show.id, show.label, show.enabled),
                ("show", "Show Scriber", true),
                "{phase}"
            );
            assert_eq!(
                (quit.id, quit.label, quit.enabled),
                ("quit", "Quit", true),
                "{phase}"
            );
            assert_eq!(
                items(phase).map(|item| (item.id, item.label)),
                [
                    ("show", "Show Scriber"),
                    ("record", "Record"),
                    ("pause", "Pause"),
                    ("done", "Done"),
                    ("discard", "Discard"),
                    ("quit", "Quit"),
                ],
                "{phase}: the pinned order"
            );
        }
    }

    #[test]
    fn each_action_item_is_enabled_exactly_when_the_core_allows_it() {
        for phase in PHASES {
            for (id, action) in [
                ("pause", Action::Pause),
                ("done", Action::Done),
                ("discard", Action::Discard),
            ] {
                let w = World::default();
                let p = w.ports();
                let mut d = session(&w, phase);
                let enabled = item(phase, id).enabled;
                let result = d.apply(action, "", &p);
                assert_eq!(result.is_ok(), enabled, "{phase}: {id}");
                if let Err(e) = result {
                    assert!(
                        matches!(e, DictationError::Illegal { .. }),
                        "{phase}: {id}: {e}"
                    );
                }
            }
        }
    }

    #[test]
    fn record_is_enabled_where_engine_record_can_start_a_session() {
        // Engine::record applies Action::New first from idle and finished, so
        // those two phases enable the item as well; Action::Record itself is
        // legal only from ready.
        for phase in PHASES {
            let enabled = item(phase, "record").enabled;
            assert_eq!(
                enabled,
                matches!(phase, Phase::Idle | Phase::Ready | Phase::Finished),
                "{phase}: the flag"
            );
            let w = World::default();
            let p = w.ports();
            let mut d = session(&w, phase);
            assert_eq!(
                d.apply(Action::Record, "", &p).is_ok(),
                phase == Phase::Ready,
                "{phase}: apply(Record)"
            );
        }
    }

    #[test]
    fn the_default_shortcut_is_pinned() {
        assert_eq!(DEFAULT_SHORTCUT, "Opt+Cmd+D", "the default");
    }

    #[test]
    fn the_default_pause_shortcut_is_pinned() {
        assert_eq!(DEFAULT_PAUSE_SHORTCUT, "Opt+Cmd+P", "the default");
        assert_eq!(
            accelerator(DEFAULT_PAUSE_SHORTCUT).as_deref(),
            Some("Alt+Super+KeyP"),
            "the plugin's form"
        );
    }

    #[test]
    fn a_shortcut_in_scriber_form_becomes_the_plugin_form() {
        for (canonical, plugin) in [
            (DEFAULT_SHORTCUT, "Alt+Super+KeyD"),
            ("Opt+Cmd+F", "Alt+Super+KeyF"),
            ("Opt+Cmd+K", "Alt+Super+KeyK"),
            ("Ctrl+Opt+Shift+Cmd+F13", "Control+Alt+Shift+Super+F13"),
            ("Opt+Cmd+Space", "Alt+Super+Space"),
            ("Opt+Shift+Cmd+Tab", "Alt+Shift+Super+Tab"),
            ("Opt+Cmd+Return", "Alt+Super+Enter"),
            ("Opt+F1", "Alt+F1"),
            ("Opt+F19", "Alt+F19"),
            ("Cmd+Escape", "Super+Escape"),
        ] {
            assert_eq!(
                accelerator(canonical),
                Some(plugin.to_string()),
                "{canonical}"
            );
        }
    }

    #[test]
    fn the_default_shortcut_is_answered_when_the_grammar_refuses_one() {
        assert_eq!(
            accelerator_or_default("Opt+Cmd+K"),
            "Alt+Super+KeyK",
            "accepted"
        );
        assert_eq!(
            accelerator_or_default("nonsense"),
            "Alt+Super+KeyD",
            "the default"
        );
    }

    #[test]
    fn anything_outside_the_grammar_is_refused() {
        for bad in [
            "",
            "D",
            "Cmd+Opt",
            "Cmd+Cmd+D",
            "Meta+D",
            "Opt+Cmd+X+",
            "Opt+Cmd+1",
            "Opt+Cmd+F20",
            "opt+cmd+d",
            "Opt+Cmd+KeyD",
            "Opt+Cmd+F0",
            "Opt+Cmd+Fx",
            "Shift+D",
            "Ctrl+D",
            "Opt++D",
            "+Opt+Cmd+D",
        ] {
            assert_eq!(accelerator(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn every_phase_has_its_icon() {
        for phase in PHASES {
            let want = match phase {
                Phase::Recording => Icon::Recording,
                Phase::Transcribing => Icon::Busy,
                Phase::Idle | Phase::Ready | Phase::Paused | Phase::Finished | Phase::Failed => {
                    Icon::Template
                }
            };
            assert_eq!(icon(phase), want, "{phase}");
        }
        assert_ne!(
            icon(Phase::Transcribing),
            icon(Phase::Finished),
            "the exit swap fires for finished"
        );
        assert_ne!(
            icon(Phase::Transcribing),
            icon(Phase::Failed),
            "the exit swap fires for failed"
        );
    }

    #[test]
    fn the_pulse_tick_is_pinned() {
        assert_eq!(PULSE_TICK_MS, 500, "two frames a second");
    }

    #[test]
    fn the_pulse_frames_alternate_only_while_transcribing() {
        assert_eq!(
            icon_at(Phase::Transcribing, 0),
            Some(icon(Phase::Transcribing)),
            "the first frame is the busy icon"
        );
        assert_eq!(
            (0..=7)
                .map(|tick| icon_at(Phase::Transcribing, tick))
                .collect::<Vec<_>>(),
            [
                Some(Icon::Busy),
                Some(Icon::Template),
                Some(Icon::Busy),
                Some(Icon::Template),
                Some(Icon::Busy),
                Some(Icon::Template),
                Some(Icon::Busy),
                Some(Icon::Template),
            ],
            "even ticks busy, odd ticks template"
        );
        for phase in PHASES {
            if phase == Phase::Transcribing {
                continue;
            }
            for tick in 0..=3 {
                assert_eq!(icon_at(phase, tick), None, "{phase}: tick {tick}");
            }
        }
    }

    #[test]
    fn the_copy_sound_path_and_the_icon_rules_are_pinned() {
        assert_eq!(
            COPY_SOUND, "/System/Library/Sounds/Pop.aiff",
            "the soft system sound"
        );
        for (icon, name, draws_as_template) in [
            (Icon::Template, "template", true),
            (Icon::Recording, "recording", false),
            (Icon::Busy, "busy", true),
        ] {
            assert_eq!(icon.name(), name, "{icon:?}: the log's name");
            assert_eq!(
                icon.draws_as_template(),
                draws_as_template,
                "{icon:?}: tinted by macOS"
            );
        }
    }

    #[test]
    fn the_copy_sound_rule_is_pinned() {
        let mut failed = finished(Some(COPIED), 7);
        failed.phase = Phase::Failed;
        for (previous, snapshot, copy_sound, want) in [
            (
                Phase::Recording,
                finished(Some(COPIED), 1),
                true,
                Sound::Play,
            ),
            (
                Phase::Recording,
                finished(
                    Some(
                        "Copied to the clipboard. Could not keep the exact audio of the parts: nope.",
                    ),
                    2,
                ),
                true,
                Sound::Play,
            ),
            (
                Phase::Recording,
                finished(Some(NOTHING_HEARD), 3),
                true,
                Sound::Notice,
            ),
            (
                Phase::Recording,
                finished(
                    Some("Could not copy to the clipboard: nope. Use Copy to try again."),
                    4,
                ),
                true,
                Sound::Notice,
            ),
            (Phase::Recording, finished(None, 5), true, Sound::Notice),
            (
                Phase::Finished,
                finished(Some(COPIED), 6),
                true,
                Sound::AlreadyFinished,
            ),
            (Phase::Recording, failed, true, Sound::NotFinished),
            (
                Phase::Recording,
                finished(Some(COPIED), 8),
                false,
                Sound::Off,
            ),
        ] {
            assert_eq!(
                sound(previous, &snapshot, copy_sound),
                want,
                "{previous} -> {}: copy_sound {copy_sound}",
                snapshot.phase
            );
        }
    }

    #[test]
    fn the_sound_reasons_are_pinned() {
        for (sound, want) in [
            (Sound::Play, "play"),
            (Sound::Off, "off"),
            (Sound::NotFinished, "not_finished"),
            (Sound::AlreadyFinished, "already_finished"),
            (Sound::Notice, "notice"),
        ] {
            assert_eq!(sound.reason(), want, "{sound:?}");
        }
    }

    #[test]
    fn the_notice_is_the_transition_into_finished() {
        assert_eq!(
            notice(Phase::Recording, &finished(Some(COPIED), 1)),
            Some(COPIED),
            "recording to finished"
        );
        assert_eq!(
            notice(Phase::Paused, &finished(Some(COPIED), 2)),
            Some(COPIED),
            "paused to finished"
        );
        assert_eq!(
            notice(Phase::Finished, &finished(Some(COPIED), 3)),
            None,
            "a later encode or summary job's snapshot"
        );
        let mut failed = finished(Some(COPIED), 4);
        failed.phase = Phase::Failed;
        assert_eq!(notice(Phase::Recording, &failed), None, "failed");
        assert_eq!(
            notice(Phase::Recording, &finished(None, 5)),
            None,
            "no notice"
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one literal and one expected line per variant, kept side by side"
    )]
    fn each_log_variant_serializes_compact_and_snake_case() {
        let lines = [
            Log::Toggle {
                phase: "recording",
                toggle: "act",
                action: Some("done"),
                outcome: "ok",
                error: None,
                phase_after: "transcribing",
            },
            Log::Pause {
                phase: "recording",
                action: Some("pause"),
                outcome: "ok",
                error: None,
                phase_after: "paused",
            },
            Log::Menu {
                id: "pause",
                phase: "recording",
                outcome: "ok",
                error: None,
            },
            Log::Shortcut {
                chord: "toggle",
                trigger: "launch",
                accelerator: "Alt+Super+KeyD",
                outcome: "registered",
                error: None,
            },
            Log::Shortcut {
                chord: "pause",
                trigger: "launch",
                accelerator: "",
                outcome: "skipped",
                error: None,
            },
            Log::Notify {
                phase: "finished",
                visible: false,
                notice: Some(COPIED),
                outcome: "shown",
                error: None,
            },
            Log::Sound {
                trigger: "finish",
                phase: "finished",
                previous: Some("recording"),
                copy_sound: true,
                reason: "play",
                outcome: "played",
                error: None,
            },
            Log::Sound {
                trigger: "settings",
                phase: "idle",
                previous: None,
                copy_sound: false,
                reason: "cached",
                outcome: "cached",
                error: None,
            },
            Log::Pulse {
                phase: "transcribing",
                tick_ms: PULSE_TICK_MS,
                frames: 0,
                last: "busy",
                outcome: "started",
                error: None,
            },
            Log::Pulse {
                phase: "finished",
                tick_ms: PULSE_TICK_MS,
                frames: 7,
                last: "template",
                outcome: "stopped",
                error: None,
            },
        ];
        let want = [
            r#"{"event":"toggle","phase":"recording","toggle":"act","action":"done","outcome":"ok","error":null,"phase_after":"transcribing"}"#,
            r#"{"event":"pause","phase":"recording","action":"pause","outcome":"ok","error":null,"phase_after":"paused"}"#,
            r#"{"event":"menu","id":"pause","phase":"recording","outcome":"ok","error":null}"#,
            r#"{"event":"shortcut","chord":"toggle","trigger":"launch","accelerator":"Alt+Super+KeyD","outcome":"registered","error":null}"#,
            r#"{"event":"shortcut","chord":"pause","trigger":"launch","accelerator":"","outcome":"skipped","error":null}"#,
            r#"{"event":"notify","phase":"finished","visible":false,"notice":"Copied to the clipboard.","outcome":"shown","error":null}"#,
            r#"{"event":"sound","trigger":"finish","phase":"finished","previous":"recording","copy_sound":true,"reason":"play","outcome":"played","error":null}"#,
            r#"{"event":"sound","trigger":"settings","phase":"idle","previous":null,"copy_sound":false,"reason":"cached","outcome":"cached","error":null}"#,
            r#"{"event":"pulse","phase":"transcribing","tick_ms":500,"frames":0,"last":"busy","outcome":"started","error":null}"#,
            r#"{"event":"pulse","phase":"finished","tick_ms":500,"frames":7,"last":"template","outcome":"stopped","error":null}"#,
        ];
        for (line, expected) in lines.iter().zip(want) {
            let json = serde_json::to_string(line).unwrap();
            assert_eq!(json, expected, "{line:?}");
            assert!(json.ends_with(expected), "compact");
        }
        // The failure side of each variant carries its sentence.
        let refused = serde_json::to_string(&Log::Menu {
            id: "done",
            phase: "idle",
            outcome: "refused",
            error: Some("Cannot done while idle."),
        })
        .unwrap();
        assert_eq!(
            refused,
            r#"{"event":"menu","id":"done","phase":"idle","outcome":"refused","error":"Cannot done while idle."}"#,
            "an error sentence"
        );
    }
}
