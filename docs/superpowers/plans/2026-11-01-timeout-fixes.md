# Timeout fixes implementation plan

> **For agentic workers:** Use superpowers:executing-plans to implement the two tasks below with regression tests first.

**Goal:** Enforce the existing request and listing time limits.
**Architecture:** Carry one absolute request deadline through client-ID coordination, scraping, the concurrency gate and per-attempt transport timeouts. Keep the listing watchdog alive through child termination, using nonblocking process polling.
**Tech Stack:** Rust 2024, std synchronization/process APIs, ureq 3.5.
**Spec:** The two timeout findings in this conversation, confirmed against 1199006.

## Global constraints
- Rust 1.85 minimum; no new dependencies.
- Work only in the isolated worktree; PR base is harden-network.
- Preserve streaming listing output and existing retry/error behavior.

## Review focus
- Queue saturation must not outlive the request deadline.
- Waiting for another client-ID scrape must be bounded and cancellable.
- A slow response body must use the remaining deadline.
- Refresh/retry paths must not restart the deadline.
- Closing stdout must not disable listing process supervision.

## Task 1: Bound the entire logical request
Files: src/api.rs, src/net.rs (including their test modules).
Interfaces: Transport::get gains a timeout Duration; Gate::enter accepts deadline and cancellation; fetch delegates to fetch_until with an absolute Instant.
- [x] Add a deadline test seam and regression tests for queued requests, client-ID lock contention, exhausted requests and a slow local HTTP body; watch failures.
- [x] Carry the deadline through scraping, refresh and retries; cap each HTTP attempt by its remaining budget; use bounded cancellation-aware gate and mutex waits.
- [x] Run cargo test --locked and commit.

## Task 2: Supervise listing until process exit
File: src/api.rs (including tests).
Interfaces: Extract listing child consumption into a helper accepting the existing silence/total limits.
- [x] Add a subprocess regression that closes stdout and remains alive; watch the timeout assertion fail.
- [x] Poll child exit without holding its mutex during a blocking wait; stop the watchdog only after exit; verify normal streamed output still works.
- [x] Run cargo test --locked, cargo fmt --all --check and cargo clippy --locked --all-targets -- -D warnings.
- [x] Review the complete diff.
- [ ] Commit, push and open a PR against harden-network.

## Execution record
- Baseline source: 1199006. User authorized implementation and PR; execute inline without another approval step.

- Baseline: 77 passed, 5 ignored.
- RED: queue, client-ID contention and slow-body tests returned success after their deadline; stdout-EOF listing test returned success after waiting for process exit.
- GREEN: shared deadline and watchdog fixes pass. Contention tests require completion while the resource remains held; cancellation is also checked during both waits.
- Independent review: fixed per-bundle timeout regression. Its regression test failed before the correction and now passes; individual timed-out bundles are skipped only while the shared budget remains.
- Validation: cargo test --locked: 85 passed, 5 ignored; mpv integration run: all 5 passed. Formatting, diff whitespace and clippy with warnings denied passed.
- Ruling: combine both fixes into one reviewable commit; they are small and share src/api.rs. No main-workspace files changed.
- Review scope: live SoundCloud behavior and pre-existing descendant-pipe hangs are not changed or claimed fixed. No dependencies added.
