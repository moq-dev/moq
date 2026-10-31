# [S] Resolve the release audit findings

## Goal

Resolve the newly identified correctness, resource, and performance risks from
the 2026-10-22 release-to-main audit before the next cut. Existing quests keep
ownership of previously tracked problems. This epic's remaining work is to
recheck the resolved findings together on the release candidate and hand the
evidence to the existing cut quest.

## Plan

The maintainer requested an extensive audit and quests for bugs, potential
issues, and performance concerns. The comparison pins are
[main 6369eca](https://github.com/moq-dev/moq/commit/6369eca26472e120ee20c473027c13d36994edad)
and [release 36f392f](https://github.com/moq-dev/moq/commit/36f392f492f571454e5abc0e441ddc0168f16477).
Their tree diff spans 3,720 files, 575,722 insertions, and 125,659 deletions.

Review covered protocol/model ownership, compression, auth, relay discovery,
QUIC/WebTransport/QMux, browser ingestion, media containers, FFI lifetimes,
archive migration, E2EE, release-only commits, and CI/publication paths.
This was risk-based source review with targeted reproductions, not a manual
verification of every changed line. Each child distinguishes reproduction,
source inference, and whether the problem is new or inherited. Priorities
describe impact; no unsupported exploit or performance magnitude is assumed.
No production code, public API, wire format, or package version changed in
the audit. Implementation choices remain with each fix; prefer the existing
ownership mechanisms and keep fixes scoped to their consumer.

Validation at these pins:

- Native protocol/auth/relay/JSON/stats: 3,830 passed, 18 skipped.
- Native task/clock/QUIC/socket: 923 passed, 2 skipped; Tokio/uring:
  656 passed, 4 skipped. A strict io_uring WebTransport echo also passed.
- Native mux/HLS/archive/E2EE: 1,992 passed, 5 skipped.
- Browser packages under Bun: 4,229 distinct tests passed across the initial
  runs and targeted reruns. Initial failures were a denied socket listen and
  two missing-libav tests; these passed after enabling loopback access and
  building libav. Hang and watch also passed their normal per-package runs.
  Combining the two polyfill tests in one custom Bun process polluted shared
  WebCodecs globals; the repository isolates packages in separate processes.
  This is not a claim that the full JS type/build/browser gate ran.
- Dependency audit reported the four already tracked unmaintained packages
  basic-toml, zbus_names, zvariant, and zvariant_derive, not four new advisories.

Run focused regression checks and relevant benchmark sweeps after the fixes,
then carry the candidate through the existing compatibility, package, draft,
and all-language interop checks. This audit did not run the full platform
matrix, real-browser WebCodecs/capture/GPU coverage, installed registry artifacts,
or a complete public-API diff against published packages. Those proofs retain
their existing owners. Reconcile release-only versions, changelogs, and
backports through the documented merge sequence before the cut.

The deeper pass advanced main to
[1d43c096e0](https://github.com/moq-dev/moq/commit/1d43c096e04431bfc87d807ce47a546df8017475),
with the same release pin. It reviewed valid-input protocol differences,
subscription bounds, JSON/model expiry ownership, HLS/archive lifecycle,
FFI/C cancellation, and the handwritten Python/Swift/Kotlin/Go/Dart/C++
wrappers. Native FFI/C checks added 371 passes and two skips. The repeated
309-test archive/HLS run passed with two skips; it is not additional distinct
coverage over the first pass.

`TMPDIR=/tmp just test interop --all` passed at the deeper pin: seven native
interop tests and the complete configured integration harness. The harness
covered Rust/Python/Go/C++/browser publishers against Rust, Python, Go, C++,
Chromium, Node, Bun, C, and GStreamer consumers, permitted and denied AUTH,
browser session refusal, CLI FETCH close, finite tails, compression, and
partial-group drops. The initial run failed because nested temporary paths
exceeded Unix socket limits in Go tooling and Chromium; the short-path rerun
completed with no skips or failures. This is Linux workspace-artifact proof,
not installed-registry or mobile/macOS/Windows validation.

Targeted reproductions confirmed a finite HLS startup race, retained HLS
server state after final drop, idle Dart stream cancellation, empty expiry
indexes, JS IETF timestamp changes, JS datagram range violations, and OBS
reordered-frame timestamps and missing EOF output. Each owning quest records
the reproduction's limits. JS reader and publisher datagram paths now enforce
subscription bounds, and the OBS source now stamps each decoded picture with its
own timestamp and drains at clean EOF.
Go reservation disposal was settled by
[moq-dev/moq#6424](https://github.com/moq-dev/moq/pull/6424): `Update(0)`
releases a reservation deterministically.
No platform-wide binding parity, full OBS runtime, fuzzing campaign, or
network-impairment performance result is implied by these checks.

The 2026-10-22 binding and OBS audit at main
[1f5caa7026](https://github.com/moq-dev/moq/commit/1f5caa7026) added the
rate recovery child. Its two OBS findings went to m2 next to the other OBS
work, because obs-moq ships on its own schedule.

## Related

- [JS subscription ranges](/quest/m1/subscribe-ranges/js.md) - range lists and frame offsets build on the bounds fixed here
- [OBS video source](/quest/m2/obs-moq-video/source.md) - the decoder replacement keeps the timing and EOF behavior fixed here
- [Measured release performance](/quest/m1/release-perf/README.md) - existing paired benchmark regressions and their confirmation runs
- [Transport performance audit](/quest/m2/audit-perf.md) - congestion-control coverage remains outstanding
- [Binding timestamps](/quest/m1/binding-timestamps.md) - exact timestamps across language APIs
- [Dependency audit](/quest/m2/audit-deps.md) - existing advisory inventory
- [Publish and adopt](/quest/m2/release/README.md) - API/feature review, merge order, candidate checks, publication, and installed-artifact proof
