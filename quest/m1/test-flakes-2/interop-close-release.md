# [XS] WebTransport publishes the acknowledged-close fix

## Goal

Waiting on moq-dev/noq: [#40](https://github.com/moq-dev/noq/pull/40) is merged
and a compatible stable `web-transport-moq` release includes its reliable
CLOSE capsule delivery fix. MoQ can then replace its 2.0.4 adapter pin
without an unpublished source override.

## Plan

Check the PR's merge commit, repository release/tag, and published crate
source together. A version newer than 2.0.4 is not proof that it includes the
fix: another release may contain only the unrelated handshake ACK change.
Use the merged commit or equivalent published source, since a squash merge
need not retain the PR head's hash.

Advance this condition through the source review and a maintainer release;
publishing requires explicit authorization. Delete this quest and its
dependency reference once the published condition is verified. The adoption
quest owns MoQ's dependency/lockfile update and final interop validation.

## Related

- [Adopt reliable refusal delivery](/quest/m1/test-flakes-2/interop-close-code.md) - consumes the published adapter and preserves the exact browser refusal assertion
