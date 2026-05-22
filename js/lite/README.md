# @moq/lite (deprecated)

> **This package has been renamed to [`@moq/net`](https://www.npmjs.com/package/@moq/net).**

The old name caused confusion because `moq-lite` is also the name of one of the wire
protocols this library speaks. The package has been renamed to `@moq/net` to make clear
that it is the **networking layer** for Media over QUIC. Under the hood it negotiates
either the `moq-lite` protocol or the full IETF `moq-transport` protocol at session setup.

## Status

`@moq/lite` now re-exports `@moq/net` so existing code keeps working without changes.
**It will not receive further updates** — new features and breaking changes ship on
`@moq/net` only. Migrate at your convenience.

## Migration

```jsonc
// package.json
{
  "dependencies": {
-   "@moq/lite": "^0.2"
+   "@moq/net": "^0.1"
  }
}
```

```ts
// Before
import * as Moq from "@moq/lite";

// After
import * as Moq from "@moq/net";
```
