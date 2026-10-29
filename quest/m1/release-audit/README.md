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

### Candidate recheck (2026-10-28)

The source candidate is
[main 487759f62a](https://github.com/moq-dev/moq/commit/487759f62a9ef39d916fe5b92df06dcff83478ce),
with
[release 7fad611aae](https://github.com/moq-dev/moq/commit/7fad611aae4ef4e7fa856ed077a0bb467f2de9be)
as the current shipping branch. Main contains the resolved audit fixes and is
usable for their combined source checks. It is not the final cut tree: release
has not been merged into main, and the existing release reviews remain open.
No release, version bump, or merge is authorized by this checkpoint.

Checks at this candidate:

- Native protocol, auth, media, relay, archive, FFI/C, and QMux suites passed:
  7,697 tests, 43 skipped. The FFI used its OpenH264 feature.
- JS lint/type/build checks and the unit-test phase passed (4,779 tests).
  Python passed 104 tests; C, C++, Kotlin, and Go checks passed. Draft checks
  and the OBS ThreadSanitizer suite passed, including reordered timestamps
  and EOF draining. These are workspace builds, not registry-install proof.
- Dart analysis and all 45 raw/wrapper tests passed, including idle stream
  cancellation. Its publish dry-run failed because the staged package inside
  ignored `.tmp/` inherited the checkout's ignore rules (hidden pubspec,
  missing LICENSE). The existing
  [moq-dev/moq#7117](https://github.com/moq-dev/moq/pull/7117) fixes that exact
  staging failure; no duplicate fix is made here. This is not evidence of a
  broken published Dart artifact.
- `just test interop --all` passed nine native interop tests and all 176
  configured harness cells: media and denied auth, finite finish, late join,
  FETCH/tails, compression, and partial-group drops. Its two indefinite Rust
  publisher runs needed forced cleanup after the stop grace period. The
  finite-finish and tail checks passed independently; all children were reaped
  and ports released. This was the default negotiated-version matrix, not the
  optional complete IETF draft matrix.
- The QMux record and CMAF declared-count benchmark sweeps passed. CMAF
  refused the hostile 4,294,967,295-sample declarations over both payload sizes.
- The release-mode track benchmark passed all 27 expiry-budget, fetch-park,
  and datagram-pressure cases. These are quick, single-candidate measurements
  on a shared host, not a new paired main-versus-release performance verdict.

The full Linux gate is not proven here: the runner-image build was denied
user namespaces, and the local flake probe rejected a scratch Unix socket.
The full native feature-matrix run was stopped in favor of the focused
regression suites above. Swift was skipped on this Linux host. Platform,
installed-artifact, public-API diff, and full OBS runtime proofs retain their
existing owners. Checks used the Nix toolchain with the inherited `BASH_ENV`
hook disabled; that host hook otherwise caused Cargo launches to fail with
`Argument list too long`.

The registry resolver selects moq-cli 0.14.9, moq-relay 0.17.9, hang 0.21.18,
`@moq/net` 0.4.7, `@moq/auth` 0.2.2, `@moq/hang` 0.5.4, `@moq/json` 0.4.3,
and `@moq/web-transport` 0.1.6. The normal compatibility guard refuses the
accepted exception because it still pins moq-cli 0.14.8. The owning
[moq-dev/moq#7061](https://github.com/moq-dev/moq/pull/7061) has already run the
full remeasurement and found failing stable published CLI controls at these
same registry pins; it leaves the exception unchanged. Do not duplicate that
fix or repin from the source-only results here.

Remaining work: repeat the Dart publish check after its existing staging fix
lands, and complete normal published-wire compatibility after its owner
resolves the stable control failure. Refresh affected source checks if the
candidate changes, then delete this epic and give the completed evidence to
the cut quest.
Recommendation: investigate and publish the corrected CLI through the existing
compatibility quest before closing this audit. Deferring that proof to the
breaking cut is a maintainer decision, not an inferred acceptance of the
published failure.

## Related

- [JS subscription ranges](/quest/m1/subscribe-ranges/js.md) - range lists and frame offsets build on the bounds fixed here
- [OBS video source](/quest/m2/obs-moq-video/source.md) - the decoder replacement keeps the timing and EOF behavior fixed here
- [Compressed tracks](/quest/m1/compress/README.md) - application migration has an existing owner
- [Measured release performance](/quest/m1/release-perf/README.md) - existing paired benchmark regressions and their confirmation runs
- [Transport performance audit](/quest/m2/audit-perf.md) - congestion-control coverage remains outstanding
- [Binding timestamps](/quest/m1/binding-timestamps.md) - exact timestamps across language APIs
- [Published-wire comparison](/quest/m0/interop-green/wire-compat-remeasure.md) - stable published controls and the planned-break refresh
- [Dependency audit](/quest/m2/audit-deps.md) - existing advisory inventory
- [Publish and adopt](/quest/m2/release/README.md) - API/feature review, merge order, candidate checks, publication, and installed-artifact proof
