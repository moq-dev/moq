# [M] E2EE lifecycle invariants hold in every API

## Goal

The rules the [E2EE epic](/quest/m2/e2ee/README.md) already decided hold in every API, not only in Rust:

- A producing generation can only be opened under an epoch it minted, so two producers can never share an epoch and repeat an AES-GCM nonce.
- A grouped write that fails after encryption ends the group, because the transport's frame index can no longer line up with the nonce.
- A datagram whose wire identity is out of range is dropped with the bad-datagram event, not thrown as a fatal error.
- The AEAD operation and byte budgets are per key, shared by every consumer of that key, without retaining state for keys a relay can make the consumer derive.
- The draft states that a relay can truncate a group undetectably, so an application does not rely on group completeness.

The Rust core already keeps the epoch unrepresentable outside the crate. The TypeScript package exports the concrete `Generation.Producer` through `export * as Generation`, and `Producer.make` is a public static that ships in the types. An app that persists an epoch and calls it twice gets two producers with independent claims, both starting at group 0 and frame 0 under the same key. The Rust grouped writer also leaves a group open after a post-encryption `write_frame` error, while the TS twin aborts.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Keep the typed generation shape in the runtime but stop exporting the concrete class: export a `Producer`/`Consumer` interface, or gate `make` behind a module-private capability from `internal.ts`. A test asserts the package surface cannot mint a producer for a fixed epoch.
- Abort the Rust group on any `inner.write_frame` error, matching `js/e2ee/src/group.ts`, and add the Rust twin of the TS test.
- Map an out-of-range sequence or an oversize payload to the dropped-datagram event in both languages, with a `2^53` sequence test in each suite.
- Share the AEAD counters across every consumer of a `(generation, physical name, domain)` key, so resubscribing cannot reset a key's failed-open or byte budget. Documenting a per-subscription reset is not an alternative: the same key is derived again, so the bound must be too.
- Bound that shared state by ownership, not eviction: tie it to the generation handle the application holds, which owns the derived keys, and refuse to re-derive a key whose accounting was discarded rather than resetting it. Settle the exact retirement rule in the PR, since `Credential::consume` can be called for epochs and names a relay chooses.
- State the mid-group truncation limit in the draft's security considerations. The audit found no way for a receiver to tell a shorter group from a truncated one; an authenticated terminal object would be a wire change and its own quest if group atomicity is ever meant to be a guarantee.
- Point the vacuous `compile_fail` doctest at the real private constructor.

The epoch section in `drafts/draft-lcurley-moq-e2ee.md` already states the rule; link it from the TypeScript docs rather than restating it.

Public API: the TypeScript entrypoint stops exporting the concrete generation class, a breaking change recorded under `## Unreleased` in a new `js/e2ee/MIGRATE.md` with the replacement interface or factory. Wire: none.

## Related

- [Encrypted browser components](/quest/m2/e2ee/browser.md) - the consumer that must not reintroduce the concrete class
- [E2EE interop](/quest/m2/e2ee/interop.md) - the cross-language proof these fixes join
