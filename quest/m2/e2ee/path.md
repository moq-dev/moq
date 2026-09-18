# [S] Opaque broadcast path

## Goal

A protected broadcast is published at `<opaque>/<epoch>`, where `<opaque>` is
derived from the credential and the application's semantic broadcast name
(`meeting.hang`) the same way track names are, and nothing in the path says
the bytes are encrypted. The `.e2ee` suffix is gone: encryption is opaque to
the path, so no matcher, exporter, recorder, or classifier grows a naming
rule, and the format after decryption stays `.hang` inside the opaque name
where only a credential holder can read it. The epoch stays as the last
segment: a subscriber needs it before it can derive anything, so it has to
travel in the clear, and the prefix is what it discovers instances under.

The draft and the vectors change together; no code moves until the Rust
and TypeScript cores take this contract.

## Plan

- `drafts/draft-lcurley-moq-e2ee.md`: rewrite {{epoch}} so the path is
  `<opaque>/<epoch>`; `<opaque>` is 22 base64url characters from a new
  derivation with label `"moq-e2ee-00 path"` over `context`, `u64(kid)`, and
  the semantic broadcast name, with no epoch, or subscribers could not find
  the prefix. Keep discovery by prefix and greatest UUIDv7. Delete the
  `.e2ee` sentences and the `.hang` prohibition; say instead that the path
  carries no format or protection marker and a plaintext consumer fails on
  the catalog it cannot find. Note in Security Considerations that the
  opaque name hides the application's name from a relay only as long as the
  application does not reuse it in plaintext, and that a relay still sees the
  epoch. The profile stays `moq-e2ee-00`: nothing has shipped.
- `drafts/moq-e2ee-00.ts` and `.json`: add a `path` label constant and a
  derivation vector for `meeting.hang` under the sample credential, and a
  negative vector for a semantic name containing `/` if the draft forbids it.
- The questline already states this contract (its README, the
  [browser](/quest/m2/e2ee/browser.md) and [CLI](/quest/m2/e2ee/cli.md)
  quests); keep them aligned with whatever the draft settles on.
- Library shape, for the cores to implement: `Credential::path(semantic) ->
  Path` beside `Generation::name(semantic) -> Name`, so the epoch-free
  derivation cannot be confused with the epoch-scoped one. Amend
  [#3717](https://github.com/moq-dev/moq/issues/3717) to record that the
  epoch segment stays and why.

Public API: none yet (the cores adopt it). Wire: none; broadcast paths are
application data.

## Closes

- [#3717](https://github.com/moq-dev/moq/issues/3717) - close this issue when the quest finishes
