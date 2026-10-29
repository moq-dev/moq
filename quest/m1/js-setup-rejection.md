# [M] A JS SETUP refusal has one rejection owner

## Goal

An IETF-only client offering its versions in a moql SETUP is refused with the
existing no-common-version error and close behavior. The expected refusal
never escapes as an unhandled promise rejection.

## Plan

Found during local full CI for
[moq-dev/moq#6983](https://github.com/moq-dev/moq/pull/6983), at
[moq-dev/moq@58027559e9](https://github.com/moq-dev/moq/commit/58027559e98fc806a0136680c6b2bbe5b1b9bd75), with Bun 1.4.2. The test
`a server refuses a client offering only IETF versions in the moql SETUP`
reported an unhandled `no common version found; client offered: ff00000e`
from `js/net/src/connection/accept.ts:187`.

The failure is not reproduced: clean current main passed the focused test,
all 44 connection-file tests, and all 2,507 net-package tests. No matching
open quest or PR was found, including deleted quest history.
A [later report](https://github.com/moq-dev/moq/pull/6997#issuecomment-6459139941)
saw the same test fail twice, intermittently, while iterating on
[moq-dev/moq#6906](https://github.com/moq-dev/moq/pull/6906).

Reproduce and trace rejection ownership before changing code. The test
creates `accepting = accept(...)`, then awaits `Stream.open` and
`ClientSetup.encode` before attaching `expect(accepting).rejects`. A late
handler is a hypothesis, not an established cause. Determine whether the
test or production owns the missing handler, and fix it at that boundary.
Preserve the existing refusal assertion, error, and close behavior; do not
add retries, sleeps, or an arbitrary run-count gate.

Public API: none expected. Wire: none expected. Update existing documentation
inline only if the eventual fix changes what a caller must do.

## Related

- [Nonfinal JS drops](https://github.com/moq-dev/moq/pull/6983) - the full check that exposed the failure; its drop behavior is independent
