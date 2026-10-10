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
error closes. Rejected: logging both at one level in moq-relay. Add a check
that fails loudly if the relay emits no close line for a connection the
harness saw open.

Public API: none. Wire: none.
