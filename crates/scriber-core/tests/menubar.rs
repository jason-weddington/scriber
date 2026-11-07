//! The menu bar's pure decisions: the toggle table, the six items and their
//! flags, the shortcut grammar, the icon, the notification rule and the log
//! lines.

#[expect(dead_code, reason = "these tests need only some of the fakes")]
mod common;

#[cfg(test)]
mod tests {
    use scriber_core::dictation::{COPIED, Dictation, DictationError};
    use scriber_core::menubar::{
        DEFAULT_SHORTCUT, Icon, Item, Log, Toggle, accelerator, accelerator_or_default, icon,
        items, notice, toggle,
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
    fn only_recording_shows_the_recording_icon() {
        for phase in PHASES {
            let want = if phase == Phase::Recording {
                Icon::Recording
            } else {
                Icon::Template
            };
            assert_eq!(icon(phase), want, "{phase}");
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
            Log::Menu {
                id: "pause",
                phase: "recording",
                outcome: "ok",
                error: None,
            },
            Log::Shortcut {
                trigger: "launch",
                accelerator: "Alt+Super+KeyD",
                outcome: "registered",
                error: None,
            },
            Log::Notify {
                phase: "finished",
                visible: false,
                notice: Some(COPIED),
                outcome: "shown",
                error: None,
            },
        ];
        let want = [
            r#"{"event":"toggle","phase":"recording","toggle":"act","action":"done","outcome":"ok","error":null,"phase_after":"transcribing"}"#,
            r#"{"event":"menu","id":"pause","phase":"recording","outcome":"ok","error":null}"#,
            r#"{"event":"shortcut","trigger":"launch","accelerator":"Alt+Super+KeyD","outcome":"registered","error":null}"#,
            r#"{"event":"notify","phase":"finished","visible":false,"notice":"Copied to the clipboard.","outcome":"shown","error":null}"#,
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
