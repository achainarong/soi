# Stabilization review

Task: c3d7a491-33ab-4cdd-aaf4-7b568001da7e

This targeted source and regression-test review covers all five requested
areas. It is not an exhaustive audit or an application-wide benchmark.

## Results

| Area | Finding | Handling |
| --- | --- | --- |
| Edge cases | Creating a plan could exceed the 512 KiB limit enforced by reading and saving. Unicode URL discovery counted characters, unlike link validation's byte limit. | Reject oversized creation before filesystem side effects; use UTF-8 byte lengths for discovered links. |
| Incompatibilities | The README promised Rust 1.88, but the locked GTK bindings require 1.92. | Declare Rust 1.92 in Cargo, correct the README, and add a 1.92.0 CI test job alongside stable on amd64/arm64. |
| Bugs | Git interpreted wildcard and pathspec-magic characters in plan filenames, reporting unrelated tracked files or failing ignore checks. | Use literal pathspecs for tracking checks and prefix ignore-check paths with `./` (check-ignore rejects literal magic too). |
| Security | Sixel command parameters could grow memory indefinitely. Huge HLS hues overflowed, panicking with overflow checks or producing wrong colors without them. Vertical positions also grew without a bound. | Store only the five supported parameters, discard surplus digits, normalize hue before arithmetic, and clamp positions beyond the canvas. |
| Performance | URL discovery repeatedly scanned the entire candidate for every trailing bracket, doing quadratic work on terminal pointer movement. | Count bracket balance once and trim in linear time. |

Regression tests cover exact and excessive plan limits, Unicode links,
mixed surrounding brackets, literal tracked/untracked/ignored filenames,
surplus Sixel parameters, extreme hues and band positions, and hostile Sixel
fed through the terminal in small output chunks. Existing public interfaces
remain the test seams; no new abstraction or adapter was necessary.

The same 30,000-trailing-bracket test measured about 342 ms before the URL
fix and 0.42 ms afterwards on this machine in the debug profile. This is a
single local stress measurement, not a claim about typical frame times.

## Validation

- `cargo test --locked --all-targets`: 184 passed, 5 ignored (including
  seven new regression tests).
- `cargo test --lib fuzz_long -- --ignored`: passed (2,000 deterministic
  seeds, 1,000 rounds each).
- `cargo test --features gui-tests --test gui`: 42 passed, 0 failed.
- Packaging Python tests: 11 passed.
- `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --check`
  and `git diff --check`: passed.

## Scope and remaining limitations

- Source review included terminal links/images/paste, Git command execution
  and worktree operations, plans, saved layouts, agent reports/launches, and
  Linux/macOS platform adapters. Existing regressions cover shell discovery,
  OpenCode 2 argument compatibility, old layout handling, bounded PTY queues,
  image budgets, and launcher/paste restrictions.
- Local validation uses Linux x86_64 and Rust 1.99. macOS runtime behavior,
  arm64, and the new Rust 1.92 CI job were not executed locally.
- The first pass had no dependency scanner; the follow-up scan below closes
  that validation gap.
- Existing limitations remain: local file handlers are not sandboxed and files
  can change between validation and launch; agent IPC trusts processes running
  as the same user; plan comparison and atomic replacement are not a filesystem
  compare-and-swap. See also [the earlier security review](security-review.md).
- No speculative process-polling or rendering refactor was made without a
  measured bottleneck. The performance fix addresses the reproduced URL case.

## Follow-up: memory lifetime and attack-surface review

### Confirmed problems fixed in code

1. **Unbounded OSC input (high-confidence denial of service).** Shells,
   remote SSH programs and displayed logs can supply terminal escape sequences.
   Although image/APC data was bounded, vte's std OSC buffer was not. An
   unterminated OSC could therefore consume arbitrary memory before any title,
   link or clipboard validation ran. The feeder now buffers at most 2 MiB,
   discards overlong/cancelled sequences without executing a prefix, and
   recovers across output chunks and following escape sequences. Valid 1 MiB
   clipboard transfers remain supported. Clipboard length checks now account
   for base64 padding and enforce the decoded limit too. Clipboard payloads
   containing NUL are rejected before GTK's C-string interface (which otherwise
   panics in debug builds or truncates the text in release builds).
2. **Closed-window reference cycles (memory leak).** Both main-window state
   and task editors owned their windows while those windows' close callbacks
   owned the state/editor. Repeatedly closing windows retained these objects;
   task editors could also retain a workspace and its polling. Accepted closes
   now release the callback's owner. Cancelled closes retain unsaved work.
   Regression checks assert weak owners, window objects and closed tabs are
   actually released, rather than treating allocator RSS caching as a leak.
   This exposed a second teardown defect: GtkStack disposal could spin forever
   with an animated popover still attached. Popovers now detach when their
   parent unrealizes, and switcher callbacks use weak popover references rather
   than retaining themselves. A repeated-open/close GUI regression checks both
   popover release and immediate parent-window teardown.
3. **Unbounded pending socket work (same-user IPC robustness).** A complete
   newline report waited for disconnect, while unlimited idle clients could
   retain connections and 64 KiB read buffers. Reads now finish at the first
   newline/EOF, have a two-second total deadline, and admit at most 32 concurrent
   clients. Size/state/NUL validation is centralized; the listener is owned and
   stopped on teardown instead of leaked with `mem::forget`. Failure to set
   user-only socket permissions now disables the listener rather than silently
   continuing. This is not a claim that other same-user processes were granted
   new privileges by the old behavior.
4. **Stubborn PTY children (process/thread leak).** portable-pty sends SIGHUP,
   not SIGKILL, from its Unix killer. A direct child ignoring HUP survived
   terminal closure, retaining a waiting reaper thread. Reapers now poll, allow
   500 ms for graceful exit, then kill and reap the still-owned direct child.
   Application exit explicitly releases terminals and gives parallel reapers
   up to two seconds to finish before exiting.
5. **Unbounded version probes (reliability hardening).** Running the configured
   OpenCode binary with `--version` could wait forever or retain unlimited
   output. Probes now have a three-second deadline, a 64 KiB stdout limit,
   null stdin/stderr, success-status validation, and direct-child cleanup.
   The configured executable is user-trusted; this is not presented as a new
   remote-code-execution vulnerability.

### Reproduction and dependency checks

- Before fixing them, isolated tests reproduced overlong OSC acceptance,
  retained closed-window state, reports stuck until disconnect, a live child
  ignoring HUP after closure, and an unbounded stalled version probe.
- `cargo-audit` 0.22.2 was installed only under `/tmp/opencode/soi-audit`.
  A fresh RustSec database (1,381 advisories) reported no advisories for the
  131 locked crate dependencies. No app dependency versions were changed.
- Final follow-up validation: 192 tests passed, 5 ignored; 47 GUI checks passed;
  the 2,000-seed long fuzz run passed; 11 packaging tests passed. All-feature
  Clippy with warnings denied, formatting and diff checks passed.

### Remaining limits

- No high-confidence arbitrary-code-execution exploit was demonstrated in this
  review. That is not proof that the app is unhackable.
- Validation uses Linux x86_64. macOS FFI, GPU-driver/native-library leaks,
  and an exhaustive heap/sanitizer audit were not exercised. Ownership and
  child cleanup have deterministic tests, not an application-wide leak-free
  certification.
- Child escalation targets the owned direct child. Deliberately detached
  descendants are not sandboxed or guaranteed to terminate; an uninterruptible
  kernel wait can exceed the shutdown deadline and is logged.
- Version detection is bounded but can still delay the first launch by up to
  three seconds. An already compromised user-selected binary can execute with
  the user's privileges regardless of version detection.
- Existing local-file protocol reads, desktop file handlers and same-user IPC
  remain trust assumptions; Soi is not an isolation sandbox.
