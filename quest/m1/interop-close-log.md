# [XS] The interop idle-out check reads the relay's close log at a fixed level

## Goal

The interop harness's idle-out check never misreads a clean close as an open
connection because of the relay's log level.

## Plan

Found while merging #5174 (2026-10-10): the relay logs a clean
`connection closed` at info and an error close at warn, and the harness
counts those lines to decide every connection closed. If the interop relay
ever runs at warn, every clean close would read as still open and fail the
round.

Decided 2026-10-10: pin the harness's relay log filter for the close-log
target to info in `test/interop/interop.sh`, so the product keeps warn for
error closes. Rejected: logging both at one level in moq-relay. Also fail
the round loudly when the relay log shows no connection for it, so a filter
that hides the `conn{id=...}` and close lines can't pass as nothing open.

Public API: none. Wire: none.

### Validation remaining

The implementation and shell regression are complete in [#5215](https://github.com/moq-dev/moq/pull/5215). Full checks and all-language interop pass with inherited `RUST_LOG=warn`. Keep this quest until the required Interop CI is green: hosted TS captured 3.27/20.06 seconds, and the independent current-main local headroom run stops at a PID 256 decode deadline (0.50/20.06 seconds). Neither failure has been attributed to relay-log accounting. Do not weaken duration or deadline checks. The stdin throughput fix in #5292 has merged; delete this quest once the required Interop run passes with it.
