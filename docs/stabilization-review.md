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
- `cargo-audit` is not installed; dependency advisories were not scanned.
- Existing limitations remain: local file handlers are not sandboxed and files
  can change between validation and launch; agent IPC trusts processes running
  as the same user; plan comparison and atomic replacement are not a filesystem
  compare-and-swap. See also [the earlier security review](security-review.md).
- No speculative process-polling or rendering refactor was made without a
  measured bottleneck. The performance fix addresses the reproduced URL case.
