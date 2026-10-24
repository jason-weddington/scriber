# Tauri design notes

Lessons from shipping Tauri 2 desktop apps on macOS, written as defaults to follow from the start. Each one came from an owner's request or a defect found in review. Where the template demonstrates a pattern, the path is given. `docs/design.md` is this app's own spec and bridge contract.

## Window and chrome

**No title strip of your own.** Use `"titleBarStyle": "Overlay"` and `"hiddenTitle": true` so content runs to the top edge and the traffic lights sit on top of it. Don't add a header bar inside the webview: that recreates the wasted strip. Give each pane a top band of the same height instead, holding that pane's real controls (a section label, buttons, a status chip), inset the left pane's band so its content starts after the traffic lights, and set `trafficLightPosition` so the lights centre on the band.

**The top bands drag the window.** Put `data-tauri-drag-region` on each band and on its plain-text children, and grant `core:window:allow-start-dragging` in the capability file. Only the band is fixed; the list below it scrolls underneath.

**No app name in the window.** The menu bar already shows it.

**Skip the translucent sidebar unless it earns its keep.** Vibrancy needs a transparent window and `macOSPrivateApi`, which the App Store rejects. The space saving comes from dropping the strip, not from the glass.

## Layout that survives real data

**Every container that holds a variable number of items scrolls, and its rows never shrink.** In a flex column, children shrink by default, so a long list gets squashed and clipped instead of scrolling. Give the scroll container `min-height: 0; overflow-y: auto` and its rows `flex-shrink: 0`. In a dialog, make the list the scrolling element (`flex: 1 1 auto; min-height: <a few rows>; overflow-y: auto`) so the heading and the action buttons stay visible however many items there are.

**Test that, generically.** Keep one helper (the reachable-items check in `apps/desktop/tests/e2e/helpers.ts`) that fills a list past a small window (30 items at 900x600), scrolls the first and last item into view, requires each to be visible (ratio 0.9, because sub-pixel heights leave a fully scrolled item a fraction short), and fails if any item's `scrollHeight` exceeds its `clientHeight` (squeezed). Run it on every list and every dialog list. It fails on exactly the bug above.

**Long text never overflows its control.** Truncate with `overflow: hidden; white-space: nowrap; text-overflow: ellipsis; min-width: 0` on the inner text span and put the full value in `title`. Add a test with an absurdly long value that asserts the control stays inside its container.

**Leave room for corner controls.** A close X in a card's corner is `position: absolute`; pad the text it could overlap.

## Controls and interaction

**Settings that apply to the whole batch live at batch level, not on every row.** If a user would set the same thing on a dozen rows, it is a batch setting. Two shapes:

- A batch default with a per-row override: one "Choose destination..." control at the top, every row follows it (rows added later too) and shows it dimmed; a row chosen by hand keeps its own value and offers "Use default". Changing the default moves only rows that follow it, never rows that are finished or in flight.
- A batch setting with no override, when per-row choice adds nothing: one checkbox in a side panel, applied to every row and to rows added later.

**The first per-row choice offers itself to the rest.** When the user sets row 1 and others are still unset, ask once: "Use this for the other 23 too?" with [Just this one] and [Apply to all].

**A new batch starts clean.** "Start over" and "Clear..." reset batch-level settings. Rows that are kept keep their own values.

**Pickers follow Finder semantics.** When the user picks where something goes, they pick the folder to put it IN; the item lands there under its own name, created if missing. Never make the user pre-create or pick the item's own target. The picker's title says what is being chosen. It opens where it was last answered (a per-user convenience in local storage, falling back to a sensible root when that place is gone).

**One long button beats a label, a field and a button.** "Choose destination..." until set, then "Moving to storage > Archive", truncating as above. Labels beside fields overlap at narrow widths.

**Small affordances for secondary actions.** Removing a row is a small X in the card's top-right corner with a tooltip and an accessible name, not a text button.

**Every list that can be emptied has a Clear...** with a confirmation that says nothing on disk is touched.

**Confirmations for destructive or bulk choices:**

- Pre-select the safe option (move to the Trash, not delete) and make the destructive button look different.
- An "Apply to all" checkbox starts unticked and lasts for the current run only.
- When something already exists with different contents, ask: Overwrite or Keep both. Keep both should leave the code that verifies and deletes unchanged (rename the thing already there so the incoming one takes the original name), and the dialog says which one gets renamed.
- Trap focus in dialogs, set initial focus, make the background `inert`, return focus to the opener.

**Every action that calls the backend has an in-flight state.** Disable Start, Retry, Remove and Clear while their call is pending; show an armed state for deferred actions ("Stop after this item"). A double click must not produce an error banner over a healthy run.

**In-window keyboard shortcuts.** Handle them on `window` keydown, map key plus phase to an action in a pure, unit-tested function that returns nothing when the phase does not allow it, and send nothing while a call is in flight or on auto-repeat. Leave Space and Return to text fields and to a button the user reached with Tab; for everything else cancel the keydown (and the Space keyup) so a button that has focus from a click is not clicked twice. Track click-versus-Tab focus yourself (pointerdown, then focusin): in Chromium any key press makes the focused element match `:focus-visible`, so it cannot tell them apart. A focused button that ends up disabled swallows every later key in Chromium, so hand the focus back to the window once the action settles. Show each shortcut on its button (`aria-hidden`) and in `aria-keyshortcuts`. Tauri's default macOS menu binds ⌘Q, ⌘W, ⌘H, ⌥⌘H, ⌘M, ⌃⌘F and the Edit items; anything else, ⌘N included, reaches the webview.

**Failed calls leave the UI truthful.** A checkbox reverts if its call fails. A failed bulk call reloads state and reports "not confirmed" rather than claiming every item failed. `init()` failures show an error with a Try again button instead of a blank window. Events that arrive while the initial snapshot loads are not overwritten by it.

**Announce state changes** (run finished, an item needs attention) in an `aria-live="polite"` region. The e2e suite runs axe on every screen, so missing names and roles fail the gate.

## Copy

- Positive and specific: "is an exact match", not "had no differences"; "Each folder", not "Each one".
- Never promise more than the backend delivers. If an ETA covers only the current item, don't say "about 3 min left, counting the check". If a "skipped" claim depends on a condition, state the condition.
- Name the real thing: derive names from the system (a mounted volume's real name from the mount table), never a hard-coded stand-in.
- Sizes use powers of 1000 like Finder and scale B, kB, MB, GB, TB, one decimal below 10, carrying 999.6 MB to "1 GB".

## Platform facts that bite on macOS

- **Wide integers.** `statvfs` block counts are 32-bit on macOS; an 18 TB volume wraps to a few hundred GB. Use `statfs` (64-bit) for free space. More generally, JSON numbers above 2^53 silently round in JavaScript: send 64-bit ids as strings and sizes as integers that stay below the limit.
- **A Homebrew tool that shadows an older system binary.** macOS ships old or differently-licensed variants of several tools (rsync is openrsync, a 2.6.9-compatible rewrite missing flags modern rsync has). If the app needs a newer one, discover it explicitly (env var, `/opt/homebrew/bin`, `/usr/local/bin`, `PATH`), check its version, accept only a qualifying one, never fall back to the system binary, document it as a README prerequisite, and replace the affected action with a clear in-app message when it is missing. Tests that need the tool fail with the install command rather than skip.
- **Inactivity timeouts and silent work.** A tool's `--timeout` is usually an inactivity timeout, and a long computation (hashing a large file) is silent. Use the timeout only on commands that stream output throughout, and otherwise rely on killing the process group.
- **Partial output.** Prefer a tool's partial-directory mode over its in-place partial mode: a killed process in the latter can rename a truncated file over the existing one.
- **Trash.** Ask Finder (the `trash` crate's default) so "Put Back" works; NSFileManager's `trashItemAtURL` avoids the Automation prompt but loses Put Back. Expect a one-time "wants to control Finder" prompt.
- **Staying awake.** `caffeinate -i -w <pid>` keeps the Mac awake for a run and dies with the app; `-i` does not stop display sleep or lid-close sleep.
- **Permission prompts are attributed to the responsible process.** An app launched from an agent session running under sshd shows its prompts as the ssh daemon's helper, and grants go to the wrong identity. Never launch, script (osascript, System Events) or screen-capture the app from an agent session: build it, install it, and let the human open it.
- **Local Network privacy.** On macOS, a process's local network access follows the GUI app it was started under. A non-Apple binary (a Homebrew or uv tool) started from an ssh session loses LAN access once the connection closes and fails with "No route to host", while Apple binaries such as curl keep working. If the app or its tools talk to LAN hosts, test from a GUI-launched context (the installed app, or a terminal app the user opened), never from an agent's ssh shell, and don't read a "No route to host" there as a network fault.
- **Metadata that moves.** File creation dates, tags and comments need explicit flags to survive a copy, and macOS pulls a creation date back to the modification date when an earlier modification date is set afterwards. If metadata preservation matters, verify it with a second pass or a check that reads it back, never assume the copy kept it.

## Process lifecycle and state

- **Quitting stops everything the app started.** Handle `RunEvent::ExitRequested` / `Exit`, signal child processes by process group (spawn them in their own group), and stop helpers like caffeinate.
- **One instance per state file.** Use `tauri-plugin-single-instance` (pinned to a version compatible with your Tauri minor) to focus the open window, plus an exclusive `flock` on a lock file next to the state file, because a dev build and the installed app are separate bundles.
- **State saves are atomic.** Write a temp file with a unique name, rename, `F_FULLFSYNC`, fsync the folder; save outside any engine lock; show save failures to the user. An unreadable file is quarantined under a timestamped name and never overwritten; older formats load through serde defaults; a fixture test loads the current format.
- **Journal destructive operations.** Persist a "removing" marker before acting, so a crash mid-operation reconciles on the next launch.
- **Strict CSP.** `tauri.conf.json` sets `default-src 'self'`, Tauri's IPC origins, no `'unsafe-eval'` or `'unsafe-inline'`, with a test that the built app runs under it (the template has one).

## Architecture that keeps it testable

- **Logic in a plain Rust crate, effects behind traits.** `crates/scriber-core` has no Tauri; clock, process spawner, host or filesystem, event sink and store are traits in `ports.rs`, with fakes in tests. `apps/desktop/src-tauri` is a thin adapter: one `#[tauri::command]` per contract entry, each forwarding to the core. `scripts/check-architecture.sh` enforces the dependency direction.
- **One bridge file.** The frontend talks to Tauri only through `apps/desktop/src/lib/bridge.ts`. In a plain browser it falls back to a deterministic mock with a fake clock, so the whole UI runs under Playwright without the app, and the mock identifies itself (`data-bridge="mock"`) so a test can prove the browser suite never hit a real backend.
- **Keep the mock honest.** Two tests do it. The mock-honesty test transcribes the core's decision rules and runs them over every case against the mock. The contract test stubs `__TAURI_INTERNALS__.invoke`, parses the `#[tauri::command]` signatures in `src-tauri/src/lib.rs`, and asserts each bridge call's command name and argument keys. Without these the mock drifts and the UI passes against a backend that does not exist.
- **Pin every bridge JSON shape with a serde round-trip test**, so a renamed field fails in Rust before the UI silently reads `undefined`.
- **The design doc is the contract** and changes in the same commit as the code.
- **Delete exactly what was verified.** A destructive step acts on the list that was checked, never on a fresh read of the directory, and re-checks the rule immediately before acting. Fail closed on anything unrecognised.

## Verification

- **App suites run when the core changes**, since the UI depends on the core's JSON shapes.
- **Playwright starts its own dev server on its own port and never reuses one.** A running `tauri dev` serves whichever checkout started it, so a worktree's tests would silently test the other checkout.
- **Fake clocks turn hangs into hot loops.** A clock that advances instantly makes any would-be hang spin at full speed, and recorders that log every sleep or event then grow without bound (one audit grew three test processes to 180 GiB and panicked the machine). Cap the recorders and abort the process past the cap, and give the test runner a hard timeout (`.config/nextest.toml`).
- **Every fix comes with a test that fails without it;** prove it by reverting the fix. Assert exact command lines for external tools, so swapped arguments fail a test.
- **Check the real app by installing it.** `scripts/install.sh` does a release build (`pnpm tauri build --bundles app`), copies the bundle into `/Applications` with `ditto`, and runs `codesign --verify --deep --strict`; the config sets `"macOS": { "signingIdentity": "-" }` so the whole bundle is ad-hoc signed. Don't run the dev server as a child of an agent session for the human to test.
- **Audit with one agent per dimension.** Each auditor is followed by a skeptical verifier, then a completeness critic and a synthesis. This found two ways to delete the only copy of user data in a codebase whose tests were green. Dimensions worth keeping: data loss, concurrency, security, external-tool integration and error handling, persisted state and recovery, performance at real scale, Rust test quality (which mutations survive), frontend test quality and mock parity, frontend code, code smells with a concrete risk, and OS and app lifecycle. Auditors run experiments only in throwaway copies, with every long command wrapped in `timeout`.

See `docs/quality-gates.md` for the gate set that backs these lessons.
