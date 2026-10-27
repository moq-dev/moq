# [S] One strict path rule, minted and parsed

## Goal

`path::Relative` refuses empty segments as `Path::new` does, the path error is
`path::Error`, and moq-auth and @moq/auth mint only roots and paths that parse
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

Public API: breaking for moq-net (`path::Invalid` becomes `path::Error`,
`Relative::new` fails on empty segments), moq-auth, and @moq/auth. Mirror the
names in JS and the bindings. Wire: none.
