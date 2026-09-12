<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/pattern

[![npm version](https://img.shields.io/npm/v/@moq/pattern)](https://www.npmjs.com/package/@moq/pattern)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Exact path patterns for [Media over QUIC](https://moq.dev): grammar, matching, and set algebra.

A pattern describes a set of broadcast paths. Tokens, origin scopes, announce interests,
and wildcard advertisements all use this package so nothing resembles a second glob dialect.
Literal paths stay coordinates; `@moq/net`'s path module keeps construction, joins, and
prefix operations.

`@moq/net` and `@moq/token` re-export these types. The Rust twin is
[`moq-pattern`](https://crates.io/crates/moq-pattern).

```bash
npm add @moq/pattern
```

```ts
import { Pattern, Patterns } from "@moq/pattern";

const grant = Pattern.parse("pid/*/chat");
grant.matches("pid/room1/chat"); // true
grant.contains(Pattern.parse("pid/room1/chat")); // true

const under = Pattern.parse("**/a").rebase("a");
[...under].map((p) => p.text); // ["", "**/a"]
```

See the package docs for the grammar, the CAT/C4M common subset, and the literal-path
rollout.
