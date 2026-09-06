# [M] Preserve QA failures as reproducible debug bundles

## Goal

A failed or hung end-to-end run leaves enough evidence to diagnose and rerun
the failure without reconstructing its environment or asking for screenshots.
Agents can inspect test processes and traces through a documented debug path.

## Plan

The smoke, WASM, and TS harnesses delete their temporary directories on exit.
The smoke and WASM workflows do not upload a diagnostic bundle. Browser console
output exists, but there is no retained Playwright trace for the failing action.

- Add a common artifact-directory convention and retain failure evidence before
  cleaning up processes. Include the command, versions, run identity, fixture
  hashes, endpoint topology, negotiated protocol, per-process logs, and results.
- Capture Playwright traces with DOM snapshots/screenshots, page errors, and
  supported browser network diagnostics. Ordinary HTTP traces do not expose
  every QUIC/WebTransport event; collect relay qlog where supported and mark
  missing backend trace capability explicitly.
- Bound logs and preserve a useful tail. On timeout, attempt a bounded stack
  capture for owned native processes before terminating them; report debugger
  permission failures without blocking cleanup. Retain matching symbols/build
  identity for crash analysis.
- Provide a local retained-session option with the exact URL, PIDs, debugger
  attach command, and teardown command. Add read-only CI artifact retrieval by
  run ID so the agent can inspect the same failure that the reviewer sees.
- Upload bundles on failure in CI with finite retention. Use synthetic media
  and test credentials by default; redact tokens, URL credentials, and headers
  before upload. Packet payloads and full core dumps require explicit opt-in.

Acceptance: inject a browser assertion failure, relay crash, and hung subscriber.
Each exits nonzero, leaves an inspectable bundle and rerun command, and reaps its
children. Verify a test token is absent from uploaded artifacts. Retrying for
diagnosis must retain the original failure rather than convert it into a pass.

[Playwright's debugging guidance](https://playwright.dev/docs/best-practices)
supports trace-based inspection; use the existing pinned Playwright dependency.

## Related

- [Worktree QA isolation](/quest/m0/worktree-qa-isolation.md) - owns run directories and process teardown
- [qlog](/quest/m1/uring-qlog.md) - adds traces for the io_uring backend; other backends can ship first
