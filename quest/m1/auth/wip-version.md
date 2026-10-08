# [M] AUTH on the wip lite version

## Goal

The lite Auth Stream (0x7) and the 0x3A UNAUTHORIZED stream code exist only
on the wip lite version (`moq-lite-07-wip` today), in Rust, JS, the draft,
and the interop matrix. A lite-06 session never opens or accepts an Auth
Stream, so no published version changes in place.

## Plan

The line built AUTH on lite-06 before the maintainer decided on 2026-10-05
that wire work targets the wip version until it is cut, never a published
version (see the README and [Finalize moq-lite-07](/quest/m1/lite07-finalize.md)).
The gate is one predicate on each side: `Version::has_auth`
(`rs/moq-net/src/lite/version.rs`) and `hasAuth` (`js/net/src/lite/version.ts`).

- Make both false for lite-06; the URL credential already covers peers
  without the stream, so a lite-06 session keeps working on its URL token.
- Move the Auth Stream and UNAUTHORIZED entries in
  `drafts/draft-lcurley-moq-lite.md` from the lite-06 changelog to lite-07's,
  and run `just drafts check`.
- Tests that negotiate the default version for AUTH must ask for
  `moq-lite-07-wip` explicitly, since the default version sets leave it out:
  `rs/moq-net/tests/auth.rs` (`LITE_06`), `js/net/src/auth.test.ts`,
  `rs/moq-relay/tests/auth_lifetime.rs`, and the AUTH rounds of
  `test/interop/interop.sh`. Add one test per language that a lite-06 session
  has no grant and opens no Auth Stream.
- Check the docs (`doc/concept`, `doc/lib/rs/moq-net.md`, `doc/lib/js/net.md`)
  name the wip version, not lite-06.

Public API: none. Wire: AUTH and UNAUTHORIZED leave lite-06; the line has not
landed, so no released peer speaks them there.

## Related

- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - the cut that publishes AUTH with lite-07
