# Changelog
All notable changes to this project will be documented in this file. See [conventional commits](https://www.conventionalcommits.org/) for commit guidelines.

- - -
## v0.7.0 - 2026-11-07
#### Features
- (**f007202e**) While local-AI post-processing runs, the status says 'Post-processing with local AI…' instead of 'Transcribing…' - (68051b4) - claude-code-sonnet, *Claude Sonnet 5.5*

- - -

## v0.6.1 - 2026-11-05
#### Bug Fixes
- (**933dce41**) Strip leaked trailing JSON braces and quotes from cleanup replies - (816176e) - claude-code-sonnet, *Claude Sonnet 5.5*

- - -

## v0.6.0 - 2026-11-05
#### Features
- (**7736710e**) Structured cleanup replacements and self-learning vocabulary loop - (8a9130f) - claude-code-sonnet, *Claude Sonnet 5.5*
#### Bug Fixes
- wrap the no-speech-was-heard warning on narrow windows - (7860066) - Jason Weddington
#### Tests
- (**cleanup**) cover boundary conditions for mutation testing - (7346191) - Jason Weddington

- - -

## v0.5.0 - 2026-11-04
#### Features
- Menu-bar dictation: a Scriber icon in the macOS menu bar with New/Record, Pause, Done, Discard and Quit, background running, and a global toggle shortcut - (8e54598) - talos-deepseek
- Settings: a custom location for the Whisper model, so several apps can share one download - (5ca81fa) - talos-deepseek
- Settings: the learned-vocabulary file's location is configurable, like the vocabulary file's - (03a9fb3) - talos-deepseek
#### Documentation
- roadmap entry for menu-bar dictation with a global shortcut - (13b17fd) - Jason Weddington, *Claude Opus 5.5*
- README example of dictation built into another app with Scriber's crates - (89a8165) - Jason Weddington, *Claude Opus 5.5*

- - -

## v0.4.0 - 2026-11-03
#### Features
- (**5b98ddd5**) extract example, round 2 for study 104: stage-2 pieces carry their own decode's full phrases, segment timeline offsets, and a repeat-decode determinism check; read afconvert's extensible float WAV - (87037d9) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**73fd283f**) scriber-engine example `extract` for applied-science study 104: decode features, Silero VAD spans and stage-2 transcripts from a recording folder - (6cfc0bf) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**c728650f**) Hidden setting keep_segments: a finished recording also keeps each part's exact WAV in its library folder - (f2ba655) - claude-code-sonnet, *Claude Sonnet 5.5*
#### Bug Fixes
- (**194eca60**) Silence profile skips the first 200 ms of each segment (device startup near-silence) - (582c71c) - claude-code-sonnet, *Claude Sonnet 5.5*
#### Documentation
- README screenshots of the ready, recording and settings screens - (4039cb4) - Jason Weddington, *Claude Opus 5.5*
- build agents mutation-test their own diff before finishing a crates/ change - (27cbb0c) - Jason Weddington, *Claude Opus 5.5*
#### Miscellaneous Chores
- release.sh also pushes main and the new tag to the public github remote - (c1edb46) - Jason Weddington, *Claude Opus 5.5*
- take LICENSE-MIT verbatim from the SPDX license list (only the copyright line filled in) - (0ba5e82) - Jason Weddington, *Claude Opus 5.5*
- license Scriber as MIT OR Apache-2.0, with both license files and a README section - (0577bbe) - Jason Weddington, *Claude Opus 5.5*

- - -

## v0.3.0 - 2026-10-31
#### Features
- (**23229a72**) Optional silence segmentation: close a segment after a sustained pause in speech so text fills in without pressing Pause - (1edb3d6) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**ab44e90d**) Learn vocabulary from the user's corrections: a word fixed in the transcript before Copy is remembered, and its spelling enforced next time - (a5fc795) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**c23e8653**) Snapshot publishes the transcript so far (partial) while recording and Whisper's raw transcript (rawText) when finished; Action::Accept learns from an edited text without a clipboard write - (d02f674) - claude-code-sonnet, *Claude Sonnet 5.5*
#### Bug Fixes
- (**c4be7824**) No layout shift: the Learned notice and every in-state message share one fixed-height status line, guarded by an e2e test - (4846557) - claude-code-sonnet, *Claude Sonnet 5.5*
- Learned-spelling loop always moves forward, and tests pin the learning and silence boundaries that mutation testing found unguarded - (4250470) - Jason Weddington, *Claude Opus 5.5*
- release.sh pushes the new version tag, which --follow-tags skipped because cog's tags are lightweight - (9f3d9d3) - Jason Weddington, *Claude Opus 5.5*

- - -

## v0.2.0 - 2026-10-31
#### Features
- (**062a2dfe**) Rework the record controls so keyboard use is shown cleanly, keeping Space, Space, Return - (32240e6) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**0b4b1915**) Make the live waveform the centrepiece of the recording screen, with its own recording colour - (7a8a4c4) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**11280e3e**) Copy becomes a two-sheets icon inside the transcript box that turns into a green check, and every button gets hover and pressed states - (c29b81e) - Jason Weddington, *Claude Opus 5.5*
- (**26d7703e**) Settings opens as a centred modal that dims the rest of the app - (df62f05) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**5b390665**) Library: summary-first rows grouped by day, and a drawer the user can resize by dragging its edge - (c46cb17) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**5e3ae279**) Move the version into Settings, remove the footer, and show the vocabulary file's name rather than a cut-off path - (bf59625) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**7515230d**) Dictated keyboard shortcuts come out as Mac glyphs (⌘, ⇧⌘P), by a deterministic rule after cleanup - (5ec3ee6) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**a3b87c88**) Show the active microphone, and let the user pick the input device from a right-click menu - (1f71c8f) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**b8eaea99**) Command-comma opens the Settings modal - (f079316) - claude-code-sonnet, *Claude Sonnet 5.5*
#### Bug Fixes
- (**4416492e**) Calm the waveform so normal speech stays inside its band, and give library day groups larger headers with a dividing line - (e9f9084) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**490712b1**) Force the cleanup reply into a JSON schema so the model's commentary cannot reach the transcript - (314beb7) - Jason Weddington, *Claude Opus 5.5*
- (**4ca4d08a**) Cleanup prompt: write clock times with a colon (9:43 a.m., not 9.43 a.m.) - (753a964) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**806d4c03**) Window chrome: remove the Dictation heading, move the drawer toggle left in full screen, space it from the traffic lights, and use a real gear for Settings - (58003f1) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**a94c33ed**) Dragging the drawer's edge no longer selects text in the library or the transcript - (f0ad96b) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**b6936526**) Trailing trim keeps quiet speech (0.002 threshold); drop a phantom last phrase only when its own audio is silent - (72868b5) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**c7874793**) Tighten the UI wave: mic menu pressed state, popover direction tests, settings alert size test, and an honest copied-state axe test - (0658a89) - Jason Weddington, *Claude Opus 5.5*
- (**ea539eb5**) Library: a thin line under every recording, replacing the line above each day header - (5a1de0d) - claude-code-sonnet, *Claude Sonnet 5.5*
- Shortcut matcher always moves forward, and a two-modifier bare letter stays spoken; mutation testing runs six jobs - (948affc) - Jason Weddington, *Claude Opus 5.5*
- Settings labels say what the local AI does: library titles, not summaries - (6cf8eba) - Jason Weddington, *Claude Opus 5.5*
#### Documentation
- tidy the two design corrections - (ce91971) - Jason Weddington, *Claude Opus 5.5*
- correct the drawer's save cadence and the settings dialog's name in the design - (dbbd753) - Jason Weddington, *Claude Opus 5.5*

- - -

## v0.1.0 - 2026-10-29
#### Features
- (**036efceb**) Settings: choose the summary model and the post-processing model from dropdowns of installed Ollama models - (a0074f6) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**6de17c4c**) Cleanup repairs self-corrections into what the speaker meant, with a new-word budget that scales with transcript length - (eff2feb) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**a796c5d7**) Keep the post-processing model loaded in Ollama while Scriber is open - (ee7e9ed) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**c98ac5b6**) Optional local-LLM cleanup pass: fix vocabulary misspellings and spoken self-corrections in the transcript - (7968bda) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**e4de7427**) Vocabulary file setting: a path with a Browse button and an Open in Text Editor button - (3bbb467) - claude-code-sonnet, *Claude Sonnet 5.5*
- list only text-generating Ollama models in the model dropdowns, hiding embedding models - (4e0d222) - Jason Weddington, *Claude Opus 5.5*
- recording library: a left drawer of past dictations titled by date and time, with local-AI summaries and m4a audio - (b083ad9) - Jason Weddington, *Claude Opus 5.5*
- rescale the level meter to real voice levels, with thin red bars sized to the width - (066f6d3) - Jason Weddington, *Claude Opus 5.5*
- extract scriber-engine, a reusable engine crate with the Tauri app as a thin client - (efd5a6c) - Jason Weddington, *Claude Opus 5.5*
- live input-level waveform, keyboard shortcuts, model auto-download and launch warm-up - (e8b0dfb) - Jason Weddington, *Claude Opus 5.5*
- record-light app icon (black, red record dot, thin red ring); SVG source in apps/desktop/icon-source - (c004f68) - Jason Weddington
- dictation with segment-wise local Whisper transcription - (1452be4) - Jason Weddington, *Claude Opus 5.5*
#### Bug Fixes
- (**3b9457e0**) Strip a leaked </think> block from Ollama replies so cleanup and summaries are not rejected - (b0aa09d) - claude-code-sonnet, *Claude Sonnet 5.5*
- (**bce60264**) Trim trailing silence from each segment before transcribing, so Whisper stops appending a phantom "Thank you." - (e56f028) - Jason Weddington, *Claude Opus 5.5*
- (**build**) set macOS 13.0 minimum so whisper.cpp compiles in the release build (Tauri's 10.13 default lacks std::filesystem) - (a85d06f) - Jason Weddington
- drop segment text with no letter or digit, so a noise-only pause adds no lone period - (3d2c3a0) - Jason Weddington, *Claude Opus 5.5*
- always prime Whisper with a punctuated style sentence so dictations come back punctuated - (a2eded5) - Jason Weddington, *Claude Opus 5.5*
- free the Whisper Metal context before exit so quitting no longer crashes - (7d5d70f) - Jason Weddington, *Claude Opus 5.5*
- bump tokio past yanked 1.54.0 and isolate the docs-only selftest from hook git env - (1734ca2) - Jason Weddington
#### Documentation
- add a roadmap with the claude -p cleanup backend under Later - (410f781) - Jason Weddington, *Claude Opus 5.5*
#### Miscellaneous Chores
- (**build**) require macOS 26 (Tahoe); Scriber targets current Apple Silicon Macs only - (b116743) - Jason Weddington
- run mutation testing only at release, not in the commit, push or full gates - (13393fd) - Jason Weddington, *Claude Opus 5.5*
- scaffold from tinderbox 0.1.0 - (cb1a835) - Jason Weddington

- - -

Changelog generated by [cocogitto](https://github.com/cocogitto/cocogitto).