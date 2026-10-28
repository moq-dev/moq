# [M] One strict path rule, minted and parsed

## Goal

`path::Relative` refuses empty segments as `Path::new` does, `Path` refuses a
non-canonical escape, the path error is `path::Error`, and moq-auth and @moq/auth mint only roots and paths that parse
strictly, so a token can never grant a path no peer can spell.

## Plan

Follows [moq-dev/moq#6648](https://github.com/moq-dev/moq/pull/6648), which
made `Path::new` strict and introduced `path::Invalid { TooLong, EmptySegment }`.

Decided 2026-10-27 (maintainer):

- `path::Relative` refuses a leading, trailing, or doubled `/`, so both path
  types share one rule.
- Rename `path::Invalid` to `path::Error`, same variants: the module's error,
  named the way `encode::Config` is.
- #6648's other choices stay: relay grant-field path errors are `Forbidden`,
  and `Path::from_static` panics on an invalid literal.
- moq-auth and @moq/auth refuse to mint a root or path that would not parse
  strictly, in Rust and JS alike.

Decided 2026-10-27 (maintainer), from
[moq-dev/moq#6927](https://github.com/moq-dev/moq/pull/6927): the moq-transport
namespace mapping must be symmetric, so `Path` refuses a non-canonical escape on
every wire rather than only at the IETF boundary.

- Today `to_tuple` (`rs/moq-net/src/ietf/namespace.rs`, and `toTuple` in
  `js/net/src/ietf/namespace.ts`) keeps a lone `\x` as `\x`, while `from_tuple`
  escapes every backslash, so the path returns as `\\x`. Both spellings are
  valid paths today, and one tuple field can decode back to only one of them.
- `Path` (and `Relative`) accept a backslash only before `\` or `/`, a new
  `path::Error` variant otherwise; `from_tuple`'s escape-every-backslash form
  stays canonical.
- Add a round-trip property test in Rust and JS: `from_tuple(to_tuple(p)) == p`
  for every valid path.

Public API: breaking for moq-net (`path::Invalid` becomes `path::Error`,
`Relative::new` fails on empty segments, and both refuse a non-canonical
escape), moq-auth, and @moq/auth. Mirror the
names in JS and the bindings. Wire: none for moq-lite, but the hang catalog
carries `Relative` in JSON (`broadcast`, `echo.path`), so a catalog spelling one
with a leading, trailing, or doubled `/` stops parsing. Our publishers never
emit those forms. @moq/net's `Path.normalizeRelative` and the hang draft's
`broadcast` field adopt the same rule.
