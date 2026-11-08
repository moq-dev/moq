# [XS] moq-relay's forged-dial test waits for bob's endpoint

## Goal

moq-relay's `socket_auth::a_forged_dial_never_connects` passes under load and runs again. Mallory
asks for bob's server endpoint only once it has reached her session, so
the request never syncs ahead of bob's announce.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete). The test failed `Check` on
[moq-dev/moq#8204](https://github.com/moq-dev/moq/pull/8204)
([run 42430262086](https://github.com/moq-dev/moq/actions/runs/42430262086)),
which touches no relay, socket, or origin code. It panicked at
`rs/moq-relay/tests/integration/socket_auth.rs:251` with
`bob's endpoint: Unroutable`. It passed 40 stress runs alone and 18 more
inside the loaded relay suite. It is quarantined with `#[ignore]` and a
`sh/gh/test-inventory.allow` entry until this quest fixes it.

Bob's first `poll_accept_tcp` only starts his server endpoint's announce,
which still has to cross bob's session, the relay, and mallory's session.
`origin::Consumer::request` reports `Unroutable` once discovery has synced
without a covering route, so mallory's request can sync first. Have the test
wait for the announce to reach mallory's origin before the forge requests
it. `.server/bob` is a hidden route (a `.`-named segment), so an announced
watch needs `with_hidden(true)` to see it. Check the file's other tests that
request a freshly announced endpoint without such a wait, and fix any that
share the race. Then remove the `#[ignore]` and the allowlist entry, and
delete this quest.

Public API: none. Wire: none.
