# Quality gates

The rule: **start strict, relax only on evidence.** Every check starts at its strictest setting, and a relaxation needs a recorded reason next to it. Quality is enforced by deterministic checks that run on every commit, because an agent (or a person) that writes code fast needs a verdict that does not depend on anyone's attention. This file says what each gate catches and what it costs; `scripts/gate.sh` is the definition.

## One gate, every surface

`scripts/gate.sh <mode>` is the only definition of the gate. lefthook calls it for the git hooks (`pre-commit`, `pre-push`), and CI or an agent harness calls `gate.sh full`. Because every surface runs the same script, an agent driven by commit hooks and one driven by a gate command are held to exactly the same bar, and the hooks cannot drift from CI. Steps run in order and stop at the first failure, naming the step.

- `pre-commit`: the per-commit checks, fast enough to run on every commit.
- `pre-push`: pre-commit plus the slow ones (coverage, doctests, mutation testing on the change, audits, e2e).
- `full`: everything, never skipped, for CI and agent harnesses; mutation testing covers the whole core.

### Hooks check, never fix

Every step is a checker; none rewrites a file. When `fmt` fails, run `cargo fmt --all` yourself and commit again. A fixer hook (`--fix`, a formatter in write mode, `end-of-file-fixer`) mutates files and exits non-zero on its first run. A person or a retrying agent absorbs that. An agent that commits once does not: the hook rewrites its finished work after the commit step, the commit fails or lands half-formatted, and the unit of work is lost or corrupted. A checker fails the same way every time and leaves the tree alone, so the failure is a clean retry trigger. Never use `--no-verify`.

### Docs-only changes skip the heavy steps

A change that touches only `docs/**` and top-level `*.md` skips clippy, tests, coverage and the rest; `scripts/docs-only.sh` decides. It fails safe: any other path, an unreadable diff or an unknown base re-enables everything. The cheap checkers (fmt, typos, gitleaks) always run. The point is that documentation edits stay cheap enough to go straight to main without anyone being tempted to skip the gate.

## The gate table

| Step | What it catches | Cost |
|---|---|---|
| `cargo fmt --check`, `cargo sort --check` | Style and manifest drift that turns diffs into noise | Seconds |
| `typos` | Misspelled identifiers and docs, which become permanent API | Seconds |
| `gitleaks` | Secrets committed once and public forever; the commit hook scans staged changes, push and full scan the tree | Seconds |
| `check-architecture.sh` | Violations of the layering rules (Tauri or async in the core, logic in the adapter, `@tauri-apps` imported outside `bridge.ts`, `#[allow]`) | Seconds |
| `selftest-checks.sh` | A custom check that has silently stopped checking | Seconds |
| `clippy` (both workspaces, `-D warnings`, `--locked`) | Bug classes the compiler accepts, plus the lint table below; `--locked` keeps the lockfile honest | Tens of seconds |
| Core tests via `cargo llvm-cov nextest --no-report` | Behaviour regressions, in an instrumented build shared with coverage | Minutes cold, seconds warm |
| Core tests under `TZ=Pacific/Chatham LANG=de_DE.UTF-8` | Hidden dependence on the machine's time zone or locale | One more test run |
| Frontend: `pnpm install --frozen-lockfile`, `svelte-check --fail-on-warnings`, `eslint --max-warnings 0`, unit tests | Type errors and warnings in templates, lint debt, a lockfile that disagrees with `package.json` | Tens of seconds |
| Rust line coverage floor (pre-push) | Code that no test executes | A test run in the warm tree |
| Doctests (pre-push) | Documentation examples that no longer compile; nextest does not run doctests | Seconds to a minute |
| `cargo mutants --in-diff`, zero survivors (pre-push) | Tests that pass without checking anything on the code just changed | Minutes |
| `cargo mutants` over the core, zero survivors (full) | The same, over everything, so nothing escapes for long | Longest step |
| `cargo machete`, `cargo deny check`, `pnpm audit --prod` | Unused dependencies, licences, advisories, unexpected registries, vulnerable frontend packages | Seconds |
| Playwright e2e with axe, frontend coverage floor, `vite build` | UI regressions against the mock, accessibility violations, untested frontend code, a bundle that does not build | Minutes |

### Why these are worth their cost

**Mutation testing** is the one gate whose cost deserves care. Coverage proves a line ran, not that anything asserted on it; a mutant (a flipped comparison, a deleted statement) that survives proves the tests would pass with the logic broken. Zero survivors is the only bar that does not erode: any tolerated number invites the next one. It is slow, so pre-push scopes it to the diff against the merge-base (untracked files included, so an agent's uncommitted work is covered), and only `full` runs the whole core. An unknown base falls back to a full run rather than skipping. Mutation copies of the tree go under `target/` rather than the system temp directory, so concurrent gates cannot fill a small tmpfs, and run few jobs in parallel.

**The time-zone and locale rerun** runs the whole core suite a second time under an unusual zone (a 45-minute offset with its own DST rules) and a decimal-comma locale. Code that formats or parses dates, numbers or sorted strings works on the author's machine and fails on a user's; the rerun catches it on the author's machine. The cost is one more test run, paid only because the suite is fast.

**One shared instrumented build tree.** Plain `cargo nextest run` and `cargo llvm-cov nextest` use different target directories, so the dependency graph and every test binary compile twice, and cargo's build lock serialises anything sharing a tree. Pre-commit therefore runs `cargo llvm-cov nextest --no-report` (build instrumented, run every test, write no report), and pre-push's coverage step reruns the tests in that same tree with `--fail-under-lines`, and doctests run with `CARGO_TARGET_DIR=target/llvm-cov-target`. Measured on a cold tree this took the combined gate down about a quarter and halved the disk used. Two traps are worth recording. Coverage must rerun the tests rather than call `cargo llvm-cov report`: the merged profile data survives the raw data being deleted, so a report-only step can pass a push on the coverage of an older commit, silently. And the instrumented build locks its own directory while clippy locks `target/`, so the two run in parallel instead of fighting over one lock (an agent that backgrounded its gate to avoid that contention lost its work when its turn ended).

**Coverage floors only ratchet up.** The floor starts at the measured value (at least 98 for Rust) and is raised whenever coverage climbs, never lowered. A floor that can move down is a suggestion. The frontend floor works the same way, measured from the Playwright runs with the mock excluded so the mock cannot inflate it.

**`#[expect(lint, reason = "...")]` instead of `#[allow]`.** `clippy::allow_attributes` and `allow_attributes_without_reason` are denied, so every exception carries a written reason, and an exception that stops being needed fails the build instead of lingering. `check-architecture.sh` also bans a bare `#[allow]` anywhere, including generated-code corners.

**The self-test proves custom checks fail.** A custom check that greps for a violation is only as good as its pattern, and a broken pattern passes everything. `scripts/selftest-checks.sh` plants one violation of each rule in a scratch copy and asserts the check exits non-zero. Every new `check-*.sh` gets a selftest case in the same commit. Run checks as scripts (`bash ./scripts/check-x.sh`), not by typing their inner commands, because `grep` in an interactive shell and `grep` in a script can be different programs with different regex dialects: use POSIX classes (`[[:blank:]]`) in anything a script executes.

**Nextest hard timeouts.** `.config/nextest.toml` sets `slow-timeout = { period = "30s", terminate-after = 4 }`, so a hung test is killed after two minutes instead of stalling the gate indefinitely. It matters most with fake clocks, which turn a would-be hang into a hot loop.

**Frozen lockfiles everywhere.** `--locked` for cargo and `--frozen-lockfile` for pnpm mean the gate builds exactly what is committed. `cargo deny` runs for each cargo workspace, because it does not look upward for a policy and the app's workspace has its own lockfile.

## The lint table

The core runs the strictest table we know, as `[workspace.lints]` in the root `Cargo.toml`; `src-tauri` is a separate workspace and carries its own copy, relaxed only where Tauri's generated code trips a lint, with a `reason`.

- **`unsafe_code = forbid`.** Memory safety is the reason to use Rust. Isolating unsafe behind a dependency is a decision, not a convenience.
- **`missing_docs`, `unreachable_pub` deny.** Public items explain themselves, and visibility is as narrow as it can be, which keeps the surface a refactor must respect small.
- **`unused_results`, `let_underscore_must_use`.** A discarded `Result` or return value is a dropped error; the lint makes the discard explicit.
- **`clippy::all` and `pedantic`.** Broad bug and clarity coverage; pedantic's noisier members are the ones that find real defects in a young codebase.
- **`unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`.** Production paths return errors; they do not abort. Tests may unwrap, via `clippy.toml`.
- **`indexing_slicing`, `string_slice`.** Out-of-range indexing and slicing mid-character panic at runtime; use `get` and checked boundaries.
- **`arithmetic_side_effects`, `as_conversions`.** Silent overflow and truncating casts produce wrong numbers with no error. This forces `checked_` and `saturating_` operations and `try_from`, and release profiles keep `overflow-checks = true` for whatever remains.
- **`wildcard_enum_match_arm`.** Adding an enum variant must force every match to decide what it means, instead of falling into a catch-all.
- **`dbg_macro`, `print_stdout`, `print_stderr`.** Debug output and stray prints do not ship; use the app's logging.
- **`shadow_unrelated`.** A shadowed name that means something else is a bug waiting for an edit.
- **`missing_assert_message`.** A failing assertion says what was being checked.
- **`tests_outside_test_module`.** Tests live where the test attributes and lint relaxations apply.
- **`clippy.toml` `disallowed-methods`** bans `std::process::exit` (skips destructors and the lifecycle handlers) and `std::env::set_var` / `remove_var` (unsound with threads).

## Relaxing a gate

Start strict and keep the setting there. Relax a gate only when you have evidence that it is wrong for this code: a measured false-positive rate, a lint that fires on generated code you cannot change, a check that costs more than the defects it has caught. Then relax the narrowest thing (one lint on one item, not the table; one file in a mutants exclusion, not the step), and write the reason next to the setting where the next reader will see it: the `reason = "..."` on the `#[expect]`, a comment above the line in `deny.toml`, `gate.sh` or `lefthook.yml`, the measured number beside a coverage floor. A relaxation with no recorded reason gets reverted. Never relax a gate to get a single commit through; fix the cause, or record why the gate is wrong.
