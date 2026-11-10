//! The menu bar's decisions, with no I/O: the tray menu's six items and which
//! of them each phase enables, what a press of each of the two global
//! shortcuts does in each phase, the three icons and the busy icon's pulse
//! frames, the notification rule, the copy sound's rule, the shortcut grammar
//! and the log line every decision writes. The desktop shell
//! (`apps/desktop/src-tauri`) only applies them; every rule lives here.
//!
//! A shortcut is Scriber's canonical form: `+`-joined tokens, zero to four
//! modifiers out of `Ctrl`, `Opt`, `Shift` and `Cmd` (no repeats), then one key
//! as `A`..`Z`, `F1`..`F19`, `Space`, `Tab`, `Return` or `Escape`, with at
//! least one of `Opt` or `Cmd`; [`accelerator`] translates it to the
//! `global-hotkey` plugin's form. [`DEFAULT_SHORTCUT`] is ⌥⌘D and
//! [`DEFAULT_PAUSE_SHORTCUT`] is ⌥⌘P.

use crate::model::{Action, Phase, Snapshot};
use serde::Serialize;

/// The shortcut a fresh install uses: ⌥⌘D. Also macOS's own Dock hide/show
/// chord, which is exactly why Settings can change it.
pub const DEFAULT_SHORTCUT: &str = "Opt+Cmd+D";

/// The shortcut a fresh install uses to pause and resume: ⌥⌘P. Another app may
/// already claim it, which is why Settings can change it.
pub const DEFAULT_PAUSE_SHORTCUT: &str = "Opt+Cmd+P";

/// What a press of each of the two global shortcuts does in a phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    /// Start a new session at once: [`crate::dictation::Dictation::record`]
    /// applies `Action::New` itself from `idle` and `finished`.
    Record,
    /// Send this action.
    Act(Action),
    /// Nothing: the session is busy or failed.
    Nothing,
}

/// The action a press of the toggle shortcut takes in `phase`.
///
/// The toggle never sends `Action::New` + `Action::Record`: `Engine::record`
/// (`crates/scriber-engine/src/lib.rs`) already applies `Action::New` itself
/// from `idle` and `finished`, and `Action::Record` is legal only in `ready`
/// (`dictation.rs`: `start(Action::Record, Phase::Ready)` -> `allow`).
///
/// `paused` resumes, because Pause can be pressed in the window or in the menu
/// and the next press of the hotkey continues that session. `transcribing`
/// and `failed` do nothing: Done has already been sent, and a failed session
/// is retried or discarded from the window.
#[must_use]
pub fn toggle(phase: Phase) -> Toggle {
    match phase {
        Phase::Idle | Phase::Ready | Phase::Finished => Toggle::Record,
        Phase::Recording => Toggle::Act(Action::Done),
        Phase::Paused => Toggle::Act(Action::Resume),
        Phase::Transcribing | Phase::Failed => Toggle::Nothing,
    }
}

/// The action a press of the pause shortcut takes in `phase`: Pause while
/// recording, Resume while paused, and nothing in every other phase. It never
/// answers [`Toggle::Record`], so a press outside recording and paused cannot
/// start a session.
#[must_use]
pub fn pause(phase: Phase) -> Toggle {
    match phase {
        Phase::Recording => Toggle::Act(Action::Pause),
        Phase::Paused => Toggle::Act(Action::Resume),
        Phase::Idle | Phase::Ready | Phase::Transcribing | Phase::Finished | Phase::Failed => {
            Toggle::Nothing
        }
    }
}

/// One item of the tray menu, in the order the shell builds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// The id the shell's menu handler matches.
    pub id: &'static str,
    /// The text the menu shows.
    pub label: &'static str,
    /// Whether the menu lets it be clicked in this phase.
    pub enabled: bool,
}

/// The tray menu's six items in `phase`, in order.
///
/// `record` is enabled in `idle` and `finished` as well as `ready`, because the
/// shell calls `Engine::record`, which applies `Action::New` first from `idle`
/// and `finished` (`crates/scriber-engine/src/lib.rs`).
#[must_use]
pub fn items(phase: Phase) -> [Item; 6] {
    [
        Item {
            id: "show",
            label: "Show Scriber",
            enabled: true,
        },
        Item {
            id: "record",
            label: "Record",
            enabled: matches!(phase, Phase::Idle | Phase::Ready | Phase::Finished),
        },
        Item {
            id: "pause",
            label: "Pause",
            enabled: phase == Phase::Recording,
        },
        Item {
            id: "done",
            label: "Done",
            enabled: matches!(phase, Phase::Recording | Phase::Paused),
        },
        Item {
            id: "discard",
            label: "Discard",
            enabled: matches!(
                phase,
                Phase::Ready | Phase::Recording | Phase::Paused | Phase::Finished | Phase::Failed
            ),
        },
        Item {
            id: "quit",
            label: "Quit",
            enabled: true,
        },
    ]
}

/// The key token as the plugin (`global-hotkey` 0.8) names it, or `None` for
/// anything Scriber's grammar does not accept.
fn plugin_key(key: &str) -> Option<String> {
    if let [b'A'..=b'Z'] = key.as_bytes() {
        return Some(format!("Key{key}"));
    }
    if let Some(n) = key.strip_prefix('F').and_then(|s| s.parse::<u8>().ok()) {
        return (1..=19).contains(&n).then(|| key.to_string());
    }
    match key {
        "Space" | "Tab" | "Escape" => Some(key.to_string()),
        "Return" => Some("Enter".to_string()),
        _ => None,
    }
}

/// Scriber's canonical shortcut, `shortcut`, as the plugin's form (`Control`,
/// `Alt`, `Shift` and `Super` in that order, then the key), or `None` when it
/// is not in the grammar [`DEFAULT_SHORTCUT`] uses.
#[must_use]
pub fn accelerator(shortcut: &str) -> Option<String> {
    let mut tokens = shortcut.split('+');
    let key = plugin_key(tokens.next_back()?)?;
    // Ctrl, Opt, Shift, Cmd, in the order the plugin names them.
    let mut modifiers = [false; 4];
    for token in tokens {
        let index = match token {
            "Ctrl" => 0,
            "Opt" => 1,
            "Shift" => 2,
            "Cmd" => 3,
            _ => return None,
        };
        if modifiers.get(index) == Some(&true) {
            return None;
        }
        if let Some(slot) = modifiers.get_mut(index) {
            *slot = true;
        }
    }
    let [ctrl, opt, shift, cmd] = modifiers;
    if !opt && !cmd {
        return None;
    }
    let mut answer = String::new();
    for (name, held) in [
        ("Control", ctrl),
        ("Alt", opt),
        ("Shift", shift),
        ("Super", cmd),
    ] {
        if held {
            if !answer.is_empty() {
                answer.push('+');
            }
            answer.push_str(name);
        }
    }
    answer.push('+');
    answer.push_str(&key);
    Some(answer)
}

/// The plugin's accelerator for `shortcut`, or the default's when Scriber's
/// grammar does not accept it. `settings::load` already keeps only an accepted
/// value, so the engine's answer is never the default unless nothing valid was
/// stored.
#[must_use]
pub fn accelerator_or_default(shortcut: &str) -> String {
    let shortcut = if accelerator(shortcut).is_some() {
        shortcut
    } else {
        DEFAULT_SHORTCUT
    };
    // `DEFAULT_SHORTCUT` is in the grammar (pinned by a test), so this answers.
    accelerator(shortcut).unwrap_or_default()
}

/// Which image the shell puts in the menu bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// `tray-template.png`: the dark/light template icon, recording off.
    Template,
    /// `tray-recording.png`: the red recording icon.
    Recording,
    /// `tray-busy.png`: the dimmed template, shown while the transcript is on its way.
    Busy,
}

impl Icon {
    /// Whether macOS should tint this icon for the light and dark menu bars (the two template-style images do).
    #[must_use]
    pub fn draws_as_template(self) -> bool {
        matches!(self, Self::Template | Self::Busy)
    }

    /// The image's name as `menubar-log.jsonl` spells it: `"template"`, `"recording"` or `"busy"`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Template => "template",
            Self::Recording => "recording",
            Self::Busy => "busy",
        }
    }
}

/// The icon for `phase`: the recording one only while recording, the dimmed
/// busy one only while the transcript is on its way, and the template one in
/// every other phase.
///
/// The shell swaps on a plain `icon(previous) != icon(now)`: the three icons
/// differ, so every entry into and exit from recording or transcribing swaps.
#[must_use]
pub fn icon(phase: Phase) -> Icon {
    match phase {
        Phase::Recording => Icon::Recording,
        Phase::Transcribing => Icon::Busy,
        Phase::Idle | Phase::Ready | Phase::Paused | Phase::Finished | Phase::Failed => {
            Icon::Template
        }
    }
}

/// How long one pulse frame shows: two frames a second, so the menu bar alternates while the transcript is on its way.
pub const PULSE_TICK_MS: u64 = 500;

/// The pulse frame for `tick` in `phase`: `None` outside `transcribing`, and
/// while transcribing `Icon::Busy` on an even tick and `Icon::Template` on an
/// odd one.
#[must_use]
pub fn icon_at(phase: Phase, tick: u64) -> Option<Icon> {
    if phase != Phase::Transcribing {
        return None;
    }
    if tick.checked_rem(2) == Some(0) {
        Some(Icon::Busy)
    } else {
        Some(Icon::Template)
    }
}

/// The soft system sound the shell plays once the finished transcript lands on the clipboard.
pub const COPY_SOUND: &str = "/System/Library/Sounds/Pop.aiff";

/// The sentence to notify when the session has just finished: the snapshot's
/// own `notice`, and only on the transition into `finished` from another
/// phase, so the later finished snapshots the encode and summary jobs report
/// notify nothing.
#[must_use]
pub fn notice(previous: Phase, now: &Snapshot) -> Option<&str> {
    if now.phase == Phase::Finished && previous != Phase::Finished {
        now.notice.as_deref()
    } else {
        None
    }
}

/// What the copy sound should do, one variant per clause of the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// The system sound is due.
    Play,
    /// The setting is off.
    Off,
    /// The snapshot is not finished.
    NotFinished,
    /// The finish transition already happened.
    AlreadyFinished,
    /// The notice is not a successful copy.
    Notice,
}

impl Sound {
    /// The `menubar-log.jsonl` reason: `"play"`, `"off"`, `"not_finished"`,
    /// `"already_finished"` or `"notice"`.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Off => "off",
            Self::NotFinished => "not_finished",
            Self::AlreadyFinished => "already_finished",
            Self::Notice => "notice",
        }
    }
}

/// Whether the copy sound is due when `now` arrives after `previous`, with the
/// setting at `copy_sound`: only on the transition into `finished` and only
/// when the notice is a successful copy, so an empty transcript, a clipboard
/// failure and a later Copy in finished stay silent.
#[must_use]
pub fn sound(previous: Phase, now: &Snapshot, copy_sound: bool) -> Sound {
    if now.phase != Phase::Finished {
        return Sound::NotFinished;
    }
    if previous == Phase::Finished {
        return Sound::AlreadyFinished;
    }
    if !copy_sound {
        return Sound::Off;
    }
    if now
        .notice
        .as_deref()
        .is_some_and(|notice| notice.starts_with(crate::dictation::COPIED))
    {
        return Sound::Play;
    }
    Sound::Notice
}

/// One line of `menubar-log.jsonl`: what each decision did, in the compact
/// `snake_case` shape `SettingsLine` already uses. The phase and outcome strings
/// are the values the shell passes (`Phase::Display`, `"ok"`, `"refused"`, and
/// so on), so a variant never guesses.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Log<'a> {
    /// A press of the global shortcut (`Engine::toggle`).
    Toggle {
        /// The phase before the press.
        phase: &'a str,
        /// `"record"`, `"act"` or `"nothing"`.
        toggle: &'a str,
        /// `"done"` or `"resume"`, only for a `Toggle::Act`.
        action: Option<&'a str>,
        /// `"ok"`, `"nothing"`, `"refused"` or `"failed"`.
        outcome: &'a str,
        /// The engine's sentence, when the call failed.
        error: Option<&'a str>,
        /// The snapshot's phase after a success, the phase before it otherwise.
        phase_after: &'a str,
    },
    /// A press of the pause shortcut (the engine's `press_pause`).
    Pause {
        /// The phase before the press.
        phase: &'a str,
        /// `"pause"` or `"resume"`, only for a `Toggle::Act`.
        action: Option<&'a str>,
        /// `"ok"`, `"nothing"`, `"refused"` or `"failed"`.
        outcome: &'a str,
        /// The engine's sentence, when the call failed.
        error: Option<&'a str>,
        /// The snapshot's phase after a success, the phase before it otherwise.
        phase_after: &'a str,
    },
    /// A click on one of the tray's items.
    Menu {
        /// The item's id.
        id: &'a str,
        /// The phase the handler read before the call.
        phase: &'a str,
        /// `"ok"`, `"refused"`, `"failed"` or `"unknown"`.
        outcome: &'a str,
        /// The engine's sentence, when the call failed.
        error: Option<&'a str>,
    },
    /// A registration attempt of one of the two global shortcuts.
    Shortcut {
        /// `"toggle"`, `"pause"` or `"both"`.
        chord: &'a str,
        /// `"launch"` in `setup`, `"settings"` after a save.
        trigger: &'a str,
        /// The accelerator string handed to the plugin.
        accelerator: &'a str,
        /// `"registered"`, `"unregistered"`, `"skipped"` or `"failed"`.
        outcome: &'a str,
        /// The plugin's error text, verbatim, when it failed.
        error: Option<&'a str>,
    },
    /// One notification decision.
    Notify {
        /// The phase that produced the decision.
        phase: &'a str,
        /// Whether the window was visible.
        visible: bool,
        /// The sentence that was (or would have been) shown.
        notice: Option<&'a str>,
        /// `"shown"`, `"skipped"` or `"failed"`.
        outcome: &'a str,
        /// The plugin's error text, when the send failed.
        error: Option<&'a str>,
    },
    /// One copy-sound decision, never carrying a notice text.
    Sound {
        /// `"finish"` in `refresh` or `"settings"` after a save.
        trigger: &'a str,
        /// The phase that produced the decision.
        phase: &'a str,
        /// The phase before it, when the line comes from a finish.
        previous: Option<&'a str>,
        /// The setting the decision read.
        copy_sound: bool,
        /// A `Sound::reason` value, or `"cached"` after a save.
        reason: &'a str,
        /// `"played"`, `"skipped"`, `"failed"` or `"cached"`.
        outcome: &'a str,
        /// Why the sound failed, when it did.
        error: Option<&'a str>,
    },
    /// One transcribing episode's pulse: a start or a stop line, never one per tick.
    Pulse {
        /// The phase that produced the line: `"transcribing"` on the start line.
        phase: &'a str,
        /// [`PULSE_TICK_MS`]: how long one frame shows.
        tick_ms: u64,
        /// The frames drawn, 0 on the start line.
        frames: u64,
        /// An `Icon::name` value: `"busy"` on the start line, and on a stop
        /// line `"busy"` means the exit swap failed and the menu bar is stuck.
        last: &'a str,
        /// `"started"`, `"stopped"` or `"failed"`.
        outcome: &'a str,
        /// Why the last frame failed, when it did.
        error: Option<&'a str>,
    },
}
