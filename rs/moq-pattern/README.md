[![Documentation](https://docs.rs/moq-pattern/badge.svg)](https://docs.rs/moq-pattern/)
[![Crates.io](https://img.shields.io/crates/v/moq-pattern.svg)](https://crates.io/crates/moq-pattern)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-pattern

Exact path patterns for [Media over QUIC](https://moq.dev): grammar, matching, and set algebra.

A pattern describes a set of broadcast paths. Tokens, origin scopes, announce interests,
and wildcard advertisements all use this crate so nothing resembles a second glob dialect.
Literal paths stay coordinates; `moq-net`'s `Path` and `@moq/net`'s path module keep
construction, joins, and prefix operations.

`moq-net` and `moq-token` re-export these types. The TypeScript twin is
[`@moq/pattern`](https://www.npmjs.com/package/@moq/pattern).

```bash
cargo add moq-pattern
```

See [docs.rs/moq-pattern](https://docs.rs/moq-pattern) for the grammar, the CAT/C4M common
subset, and the literal-path rollout.
