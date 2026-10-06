# [S] Import quinn-udp into moq-sock

## Goal

quinn-udp from quinn-rs/quinn `main` lives in `moq-sock` as a module
(`moq_sock::udp` is the recommendation), carrying the GSO resend fix below
with its regression tests, and moq-uring's `udp_tokio` benchmark uses it
instead of `moq-noq-udp`. It is not a separate crate.

## Plan

Decided 2026-10-06: quinn-udp joins `moq-sock` because both runtimes consume
it (moq-tokio through quinn's async layer in the
[switch](/quest/m1/quic/fork/switch.md), moq-uring for its benchmark), and
`moq-sock` already holds their shared socket plumbing. Import from the
quinn commit `rs/moq-quic` records (or newer, cherry-picking quinn-proto
up to match) so the two stay in step.

Keep the first commit verbatim from upstream so a reviewer can diff it;
module paths, lints, and wiring go in later commits. Keep quinn's formatting
for the imported files (a `.rustfmt.toml` scoped to the module directory) and
extend the cherry-pick recipe in `rs/moq-quic/README.md` to map
`quinn-udp/src/` onto the module.

Carry [quinn#2724](https://github.com/quinn-rs/quinn/pull/2724) as its own
cherry-pick commit: when the kernel rejects a GSO batch with `EIO` or
`EINVAL`, quinn-udp halts GSO and drops the batch, so an Android client's
Initial waits a full RTO and the WebSocket fallback wins the race; the fix
resends it as individual datagrams. Then extend it in a separate commit:
upstream resends only the batch that halted GSO, so a batch another
connection built before the flip falls through to the `IP_TOS` fallback
(latching it for the socket) and is dropped. Resend every rejected batch that
has a segment size, using `max_gso_segments.swap(1, ..)` so only the first
rejection logs. Both landed and were reviewed on the abandoned
`moq-quic-udp` crate in #4879 (`fb27ffb88`, `058c88e22`, `d646b6265`); reuse
them. Accepted limitation: a `WouldBlock` partway through the individual
resend makes the caller retry the whole batch, duplicating the datagrams
already sent; it only affects batches in flight when GSO halts, and QUIC
drops the duplicates.

Regression tests, both Linux-only: `SO_NO_CHECK` on the sending socket makes
Linux reject every `UDP_SEGMENT` send with `EINVAL` while plain sends go
through. `gso_rejected_batch_is_resent` sends one batch of two segments and
receives both; it fails without quinn#2724. `gso_stale_batch_is_resent`
sends the same GSO transmit twice on a blocking socket, asserting
`max_gso_segments() == 1` between them, and receives all four datagrams; it
fails without the extension.

List both under "Carried changes" in `rs/moq-quic/README.md`, and drop them
if upstream lands [quinn#2748](https://github.com/quinn-rs/quinn/pull/2748)
and we cherry-pick that. Add quinn-udp advisories to the README's triage.
