<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/pattern

[![npm version](https://img.shields.io/npm/v/@moq/pattern)](https://www.npmjs.com/package/@moq/pattern)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Exact path patterns and prefix grants for [Media over QUIC](https://moq.dev): grammar,
matching, and set algebra.

A `Pattern` describes a set of broadcast paths, for discovery filters that pick which
announcements a reader looks at. A `Prefix` is a grant member: a path covering itself and
everything beneath it, the empty prefix covering everything. `Prefixes`, a reduced set of
them, is what tokens and origin scopes grant. A grant takes no wildcard: `foo/**` and `**`
are refused, written `foo` and `""` instead.
Literal paths stay coordinates; `@moq/net`'s path module keeps construction, joins, and
prefix operations.

`@moq/net` and `@moq/auth` re-export these types. The Rust twin is
[`moq-pattern`](https://crates.io/crates/moq-pattern).

```bash
npm add @moq/pattern
```

```ts
import { Pattern, Prefixes } from "@moq/pattern";

const grant = Prefixes.parse(["pid/room1", "pid/room2"]);
grant.matches("pid/room1/chat"); // true
grant.overlaps("pid"); // true
grant.rebase("pid").toJSON(); // ["room1", "room2"]

const filter = Pattern.parse("pid/*/chat");
filter.matches("pid/room1/chat"); // true

const under = Pattern.parse("**/a").rebase("a");
[...under].map((p) => p.text); // ["", "**/a"]
```

See the package docs for the grammar, the CAT/C4M common subset, and the literal-path
rollout.
