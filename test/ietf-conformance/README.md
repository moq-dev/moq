# IETF relay conformance

Run `nix develop --command just test ietf-conformance` on Linux. The harness
builds the Moxygen revision and matching Meta dependencies pinned in
[`nix/moxygen-sources.nix`](../../nix/moxygen-sources.nix), then runs its
`conformance_test.sh` through this checkout's relay. This is separate from the
published-package smoke matrix.

Sections 1-7 run on both shared drafts, 16 and 18, through one relay and through
two relays joined with native lite-07. Each cell runs 29 upstream SUBSCRIBE
cases. A direct-origin control runs the same cases once per draft, separating
suite or pin failures from relay behavior. FETCH is skipped using upstream's `SKIP_FETCH`; section 8 uses PUBLISH
and is outside the relay's support. Cases using forwarding modes 1 or 2 send
nonzero subgroups and are explicitly excluded. Modes 0 and 3 are retained.
The staged script preserves every selected case's arguments and deadline.
A client that crashes after printing SUCCESS still fails. Selection tests prove
these exclusions and failure sensitivity, and refuse a changed script anchor.

The publisher advertises the `moq-test-00` prefix. The client subscribes to
16-field namespaces below it. Mock-clock integration tests in
[`ietf_conformance.rs`](../../rs/moq-net/tests/integration/ietf_conformance.rs)
prove this routing path on both drafts and topologies before the live suite.
The live harness waits for that prefix at the subscriber relay before starting
cases and rejects upstream publisher reconnects, so a recovered session cannot
hide a failure.

The nightly workflow runs this recipe. `MOQ_TEST_KEEP=1` retains the upstream
reports, client and server logs, relay logs, configs, and endpoints. All children
and port reservations belong to the shared harness and are reaped on exit.
The pinned C++ build is opt-in under `legacyPackages.<system>.moxygen`; ordinary
checks and development shells do not build it. Updating the pins also requires
reviewing the selected cases and rerunning the full matrix. The build recipe
adds the client's missing Folly ManualExecutor link dependency and patches
dependency script interpreters for Nix; it changes no protocol code.

## Contract decision

The full command currently fails against the relay. Its verdict is preserved:
the upstream client requires a priority that alternates by group, while this
relay deliberately ranks egress by the track's priority. It also rejects the
relay's END\_OF\_TRACK status. Keeping that status is a settled decision in
[the runner decision](https://github.com/moq-dev/moq/blob/5f09861634b793056f840408a589f89a8c464fb0/quest/m1/interop-coverage/runner-cases.md).

The [passthrough quest](../../quest/m1/ietf-extension/README.md) retains the unmet
Goal and the remaining conformance-profile decision. This draft does not change
protocol behavior or hide these differences behind extra exclusions. The nightly
job will remain red until that conflict is resolved.

The two-relay path also needs metadata from TRACK\_STATUS before subscribing.
The pinned MoQTestPublisher inherits NOT\_SUPPORTED for that request. The relay
preserves that refusal, rather than widening query-only demand into SUBSCRIBE.
The [range-widening quest](../../quest/m1/compress/ietf-range-widen.md) owns that
separate design boundary.

Measured against relay base `5f09861634`:

| Draft | Direct origin | One relay | Two relays |
| --- | --- | --- | --- |
| 16 | 29 pass, 0 fail | 0 pass, 29 fail | 0 pass, 29 fail |
| 18 | 28 pass, 1 fail | 0 pass, 29 fail | 0 pass, 29 fail |

The direct draft-18 failure is upstream's "Large scale: Many groups and objects"
case: `Group Mismatch: Actual=1 Expected=0`. One single-relay draft-18 case also
stops on that diagnostic. The other single-relay failures report the priority
and terminal-status differences above; END\_OF\_GROUP header mismatches name only
the added terminal stream at `last_group + 1`. Neither run reports a missing
FIRST\_OBJECT signal. Both cluster cells refuse the metadata query before media
flows. These results establish prerequisites and a contract conflict, not a
passing data-preservation result or a completed quest.
