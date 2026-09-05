<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/path

[![npm version](https://img.shields.io/npm/v/@moq/path)](https://www.npmjs.com/package/@moq/path)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Path patterns: the one wildcard grammar every predicate over a MoQ broadcast path uses.

A broadcast path is `/`-separated segments. A pattern describes a set of them: a literal segment matches itself, `*` matches exactly one segment, and `**` matches any run of zero or more segments (at most once per pattern). Wildcards are whole segments, so `foo/*` matches `foo/bar` but not `foo/bar/baz`, `foo/**` matches both, and `**` matches everything. Patterns are exact: `foo` matches only `foo`, and a subtree is written `foo/**`.

The algebra is what authorization and routing need: `contains` decides whether a grant covers a request, `overlaps` whether two patterns share a path, `specificity` ranks the patterns matching one path, and `rebase` rewrites a pattern relative to a literal root. It matches the Rust [`moq-path`](https://crates.io/crates/moq-path) crate byte for byte, checked by shared test vectors.

## Quick Start

```bash
npm add @moq/path
```

```ts
import { Pattern, Patterns } from "@moq/path";

const transcode = Pattern.parse("**/transcode.pro");
transcode.matches("pid/foo.hang/transcode.pro"); // true
transcode.matches("pid/foo.hang"); // false

const grant = Pattern.parse("pid/**");
grant.contains(Pattern.parse("pid/*/chat")); // true
grant.contains(transcode); // false

// A union of patterns, reduced so no member contains another.
const scope = new Patterns([Pattern.parse("pid/a"), Pattern.parse("pid/*")]);
scope.size; // 1
```

## License

Licensed under either:

- MIT License ([LICENSE-MIT](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/moq-dev/moq/blob/main/LICENSE-APACHE))
