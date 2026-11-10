#!/usr/bin/env bash
# The architecture rules in docs/design.md, as checks. Each rule prints what
# it found, and the script exits non-zero if any rule fails.
# scripts/selftest-checks.sh plants a violation of each one and requires this
# script to catch it.
set -uo pipefail
cd "$(dirname "$0")/.."
fail=0
bad() { echo "check-architecture: $*" >&2; fail=1; }

core=crates/scriber-core
app=apps/desktop/src-tauri
ui=apps/desktop

# Non-comment lines of the Rust files under a directory, as file:line:text.
rust_code() { grep -rnE --include='*.rs' "$1" "$2" | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//'; }

# 1. Dependency direction: the core has no Tauri, no webview and no async
#    runtime anywhere in its normal dependency tree.
if deps=$(cargo tree --locked -q -p scriber-core -e normal --prefix none 2>/dev/null | awk '{print $1}' | sort -u); then
  for banned in tauri tauri-build wry tao tokio async-std smol futures; do
    grep -qx "$banned" <<<"$deps" && bad "scriber-core depends on $banned"
  done
  grep -qE '^tauri-' <<<"$deps" && bad "scriber-core depends on a tauri crate"
else
  bad "cargo tree failed for scriber-core"
fi

# 1b. The engine is a library for any Rust app: the same ban as the core, so
#     no Tauri, webview or async runtime reaches a consumer through it.
if deps=$(cargo tree --locked -q -p scriber-engine -e normal --prefix none 2>/dev/null | awk '{print $1}' | sort -u); then
  for banned in tauri tauri-build wry tao tokio async-std smol futures; do
    grep -qx "$banned" <<<"$deps" && bad "scriber-engine depends on $banned"
  done
  grep -qE '^tauri-' <<<"$deps" && bad "scriber-engine depends on a tauri crate"
else
  bad "cargo tree failed for scriber-engine"
fi

# 2. No direct I/O in the core: every effect goes through a trait in ports.rs.
io='std::(fs|net|process|env|thread::sleep)|\b(File|OpenOptions|TcpStream|Command)::|\b(Instant|SystemTime)::now|\btokio\b'
if hits=$(rust_code "$io" "$core/src"); then
  bad "direct I/O in the core (use a port in ports.rs):"$'\n'"$hits"
fi

# 3. src-tauri is a thin adapter: the library and the main shim only, the
#    library small enough to read in one sitting, and no tests (logic that
#    needs a test belongs in the core). The shell now has a second client
#    surface, the tray, but still holds no decision logic: every rule lives in
#    scriber-core/src/menubar.rs.
extra=$(find "$app/src" -name '*.rs' ! -path "$app/src/lib.rs" ! -path "$app/src/main.rs")
[ -z "$extra" ] || bad "src-tauri holds more than lib.rs and main.rs (move logic to the core): $extra"
lines=$(wc -l < "$app/src/lib.rs")
[ "$lines" -le 500 ] || bad "$app/src/lib.rs is $lines lines (limit 500): move logic to the core"
if hits=$(rust_code '#\[(cfg\(test\)|test)\]' "$app/src"); then
  bad "tests in src-tauri (test the logic in the core instead): $hits"
fi

# 4. bridge.ts is the only frontend file that imports Tauri.
if hits=$(grep -rnE "(from|import\()[[:space:]]*['\"]@tauri-apps/" "$ui/src" "$ui/tests" | grep -v "^$ui/src/lib/bridge.ts:"); then
  bad "only src/lib/bridge.ts may import @tauri-apps:"$'\n'"$hits"
fi

# 5. Outside Tauri the bridge falls back to the mock, and the mock says so.
grep -q "dataset.bridge = 'mock'" "$ui/src/lib/bridge.ts" \
  || bad "bridge.ts no longer marks the mock with data-bridge=\"mock\""

# 6. No #[allow(...)] anywhere: exceptions are #[expect(lint, reason = "...")].
if hits=$(grep -rnE --include='*.rs' '#!?\[allow\(' crates "$app"); then
  bad "use #[expect(lint, reason = \"...\")], not #[allow]:"$'\n'"$hits"
fi

exit $fail
