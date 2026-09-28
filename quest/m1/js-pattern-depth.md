# [XS] JS subtree at max depth

## Goal

`Pattern.subtree` in `@moq/pattern` accepts a 32-segment path (the wire path
limit) and returns the literal path, as Rust's `Pattern::subtree` does since
https://github.com/moq-dev/moq/pull/4284. A 33-segment path still throws
`TooManySegments`.

## Plan

`js/pattern/src/index.ts` always appends `**`, so a max-depth path becomes 33
segments and throws, which empties an announce subscription under that prefix
just as it did in Rust. Nothing can sit beneath a max-depth path, so the path
itself is the whole subtree.

The Rust regression is a unit test in `rs/moq-pattern/src/pattern.rs`, which
JS never runs. Move the max-depth case (and the 33-segment refusal, if the
vector shape grows an error field the way `literal` has one) into the shared
`rs/moq-pattern/tests/pattern.json`, so both languages run the same vector
and the next divergence fails in whichever side lags.
