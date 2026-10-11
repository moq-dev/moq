# [M] Initial catalog acquisition survives a usable transport fallback

## Goal

Fix the initial `catalog.json` routing failure observed by TS validation,
with a deterministic regression and an automated round trip. A publisher
remaining available on a usable path must not leave the subscriber with a
spurious `Unroutable`; genuine refusal or disappearance still fails explicitly.

## Plan

Decided with the maintainer on 2026-10-11: reproduce and fix the cause, not
stop at diagnosis. This is independent of the pair grader's acquisition
contract. It is not a general transport redesign or a TS-specific retry path.

Local live, EIT and HRD runs during #5328 failed their initial `catalog.json`
track-info request shortly after selecting WebSocket fallback. The exporter
exited one with `moq: unroutable`; the publisher continued to normal EOF and
exited zero. The live grader received zero packets. Live and EIT ran
concurrently; HRD reproduced later. Do not count these as three independent
sequential reproductions. The full PR TS job passed, including EIT and HRD;
its event skipped the live and pair arms. No transport root cause is established.

Trace the failing initial request across publication discovery, catalog
availability, route selection and any session replacement. Preserve enough
session/broadcast/request identity to distinguish a real unavailable route
from a stale handle or handover race. First check current main and #5284:
that PR preserves a pending QUIC dial after an early WebSocket close, but
similar timing is not proof that it fixes this failure. If it owns the same
cause, add the missing regression there and verify the landed fix instead of
creating competing implementations. Otherwise keep the fix focused on the
layer that loses the usable route.

Turn the observed ordering into a controlled regression before changing
behavior. Hold or order the relevant events and mock time where needed;
do not rely on load, sleeps, repeated reruns or larger timeouts to reproduce
or hide the failure. Cover the successful fallback/acquisition path and the
adjacent terminal path so a fix cannot silently retry genuine refusal or
keep a disappeared publication alive. Preserve existing broadcast/track
identity and epoch semantics.

Verify the affected path with the TS round-trip harness and its existing
process-status, nonempty-content and decoder assertions. A successful rerun
alone does not finish the quest. Wire every regression and any new controls
into CI, with long live coverage at least nightly; retain diagnostic logs
and failing fixtures. Keep the publisher and exporter statuses in the verdict.

Keep the older short-duration capture regression and midstream `--stitch`
catalog-linger work separate unless reproduction establishes the same cause.
Update comments and affected feature docs with the fix. No new guide, public
API or wire change is expected; a newly required contract needs a maintainer
decision rather than an invented retry or compatibility layer.

### Evidence and remaining decision

Controlled reproduction in `rs/moq-net/tests/catalog_acquisition.rs`: hold the
publisher's initial catalog track request, introduce a cheaper second session to
the same publisher, observe the route change, then answer the held request.
Lite-06 returns `Unroutable`; Lite-07 with the publisher's explicit epoch acquires
and reads the catalog. The old session and publisher both remain alive. No
wall-clock timing, retry or transport teardown is needed.

This follows the current identity contract: an epochless route replacement is a
new instance, and a pending `TrackInfoFetch` checks its captured
`RequestValidity` before accepting a response. Default negotiation excludes the
work-in-progress Lite-07 wire. #5284's closed-fallback case is different: this
reproduction has no closed session. The reproduction establishes a sufficient
ordering for the reported error, not proof that all retained historical captures
followed exactly that ordering.

A fresh pair run also failed with the explicit replacement-broadcast error during
upgrade; a subsequent traced direct-WebTransport run passed table alignment.
The pair grader fix is #5331 and does not change routing semantics.

The maintainer must settle the newly exposed contract before a production fix:
use epoch-capable negotiation for seamless acquisition/continuation, or permit a
fresh initial acquisition on a replacement before output begins. Existing handles
must never silently cross publisher identities. Keep #5328 draft; do not paper
over the default behavior with retries, `--stitch`, or disabled fallback.

## Related

- [Validation PR #5328](https://github.com/moq-dev/moq/pull/5328) - records the process failures and successful PR CI baseline
- [Closed fallback](/quest/m1/transport-upgrade/closed-fallback.md) - #5284 owns the pending QUIC dial after an early WebSocket close
- [Fallback PR #5284](https://github.com/moq-dev/moq/pull/5284) - check its actual effect on this reproduction before duplicating work
- [Transport upgrade](/quest/m1/transport-upgrade/README.md) - existing handover and epoch contracts
- [TS duration fidelity](/quest/m0/ts-duration-fidelity.md) - previously captured only a short suffix, not zero-packet initial acquisition
- [Replacement catalog linger](/quest/m1/ts-follow-catalog-bound.md) - bounds a midstream stitch, not initial discovery
- [Pair acquisition](/quest/m1/ts-pair-acquisition.md) - independent grader/mux finding
