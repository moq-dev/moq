# IETF relay conformance

Run `nix develop --command just test ietf-conformance` on Linux. The harness
builds the Moxygen revision and matching Meta dependencies pinned in
[`nix/moxygen-sources.nix`](../../nix/moxygen-sources.nix), then runs its
`conformance_test.sh` through this checkout's relay. This is separate from the
published-package smoke matrix.

Sections 1-7 run on both shared drafts, 16 and 18, through one relay, and on
draft 18 through a two-relay cluster joined by native lite-07. Draft 16 has no
cluster cell: its TRACK\_STATUS\_OK has no Track Properties block, so the
cluster's origin cannot learn the track's priority from it. Each cell
runs 29 upstream SUBSCRIBE cases. A direct-origin control runs the same cases
once per draft, separating suite or pin failures from relay behavior. FETCH is
skipped using upstream's `SKIP_FETCH`; section 8 uses PUBLISH and is outside the
relay's support. Cases using forwarding modes 1 or 2 send
nonzero subgroups and are explicitly excluded. Modes 0 and 3 are retained.
The staged script preserves every selected case's arguments and deadline.
A client that crashes after printing SUCCESS still fails. Selection tests prove
these exclusions and failure sensitivity, and refuse a changed script anchor.

The publisher advertises the `moq-test-00` prefix. The client subscribes to
16-field namespaces below it. Mock-clock integration tests in
[`ietf_conformance.rs`](../../rs/moq-net/tests/integration/ietf_conformance.rs)
prove this routing path on both drafts, through one relay and through two
relays joined with native lite-07. The live harness waits for that prefix at the
relay before starting
cases and rejects upstream publisher reconnects, so a recovered session cannot
hide a failure.

The nightly workflow runs this recipe. `HARNESS_KEEP=1` retains the upstream
reports, client and server logs, relay logs, configs, and endpoints. All children
and port reservations belong to the shared harness and are reaped on exit.
The pinned C++ build is opt-in under `legacyPackages.<system>.moxygen`; ordinary
checks and development shells do not build it. Updating the pins also requires
reviewing the selected cases, rebasing the profile patch, and rerunning the full
matrix. The build recipe adds the client's missing Folly ManualExecutor link
dependency and patches dependency script interpreters for Nix.

## Conformance profile

The client runs under a relay conformance profile, applied by
[`nix/moxygen-profile.patch`](../../nix/moxygen-profile.patch). The test origin
alternates priority by group and never ends the track in-band. This relay ranks
egress by the track's priority and keeps END\_OF\_TRACK, as decided in
[the runner decision](https://github.com/moq-dev/moq/blob/5f09861634b793056f840408a589f89a8c464fb0/quest/m1/interop-coverage/runner-cases.md).
The profile therefore accepts:

- the track's advertised publisher priority (200) on any group, besides the
  per-group value;
- one END\_OF\_TRACK on the subscription, directly after the last object,
  either inline or opening the next group. That terminal stream skips the
  END\_OF\_GROUP header expectation, since it ends no payload group.

Every other check is upstream's: forwarded object IDs, properties, payloads,
END\_OF\_GROUP status objects and header bits, and FIRST\_OBJECT. Any other
priority, a second END\_OF\_TRACK, or one anywhere else still fails.

Measured over three consecutive runs, after six earlier runs of the
direct-origin and one-relay cells:

| Draft | Direct origin | One relay | Two relays |
| --- | --- | --- | --- |
| 16 | 29 pass | 29 pass | not run |
| 18 | 29 pass | 29 pass | 29 pass |

## Local Moxygen patches

A cluster's edge asks the origin about a track with TRACK\_STATUS before
subscribing.
[`nix/moxygen-track-status.patch`](../../nix/moxygen-track-status.patch) makes
the pinned MoQTestPublisher answer it with what its SUBSCRIBE\_OK carries,
instead of NOT\_SUPPORTED, and stops Moxygen writing a GROUP\_ORDER parameter
in TRACK\_STATUS\_OK, which draft 18 forbids there. Drop it at the next pin
bump once
[facebookexperimental/moxygen#264](https://github.com/facebookexperimental/moxygen/pull/264)
is in the pinned revision.

## Known verifier flake

One early run of the direct draft-18 control failed upstream's "Large scale:
Many groups and objects" with `Group Mismatch: Actual=1 Expected=0`, without
any relay. It has not recurred since. The verifier keeps one cursor per
subscription and expects groups in order, although each group arrives on its
own stream; its own comment calls that a spurious failure. If it recurs in
nightly, fix the verifier in the profile patch with per-group cursors rather
than retrying.
