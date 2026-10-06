# [XS] Import catalog finish

## Goal

`moq-cli`'s `import_delivers_the_catalog_finish_at_eof`
(`rs/moq-cli/tests/import.rs`) passes under a loaded `just check`. It runs
8.8 s alone against a 10 s timeout and fails with "announce timed out" under
load.

## Plan

Preserve the real `moq` subprocess coverage: closing stdin must finish the
catalog and let the process exit successfully while the subscriber still
receives a clean finish. Pausing the parent's Tokio clock cannot control the
child process, and the real QUIC sockets can race an auto-advancing clock.
Reproduce the slowdown, fix its cause, and coordinate the fixture through
observable readiness and completion events rather than elapsed-time
assumptions. Use an in-memory fixture for separable protocol assertions if
helpful, while retaining the subprocess EOF regression. Never raise the
timeout or add a retry.

Public API: none. Wire: none.
