<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/ui

[![npm](https://img.shields.io/npm/v/@moq/ui)](https://www.npmjs.com/package/@moq/ui)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

The base of the `<moq-watch-support>` and `<moq-publish-support>` banners in [@moq/watch](../watch) and [@moq/publish](../publish), built on [@moq/signals](../signals).
You only need it directly to build a support banner for your own capability checks.

`Support` is an abstract `HTMLElement` that renders a banner and details table for the `Report` its `detect()` returns. Its `show` attribute (`Show`) picks when the banner appears, by the report's `Level`.
The overlay helpers the two packages share (control buttons, stats cards, sparkline, formatters) live here too, but are internal and may change in any release.

It holds no capability detection and publishes no styles: each overlay styles these elements and detects its own support.
Inside this repository, `@moq/watch` and `@moq/publish` inline the shared stats card and sparkline styles from `src/stats.css` at build time; that file is not part of the npm package.

```bash
bun add @moq/ui
# or
npm add @moq/ui
```

```ts
import { type Report, Support } from "@moq/ui";

class MySupport extends Support {
	protected async detect(): Promise<Report> {
		const webcodecs = typeof VideoDecoder !== "undefined";
		return { summary: webcodecs ? "full" : "none", rows: [["Video", "WebCodecs", webcodecs]] };
	}
}
customElements.define("my-support", MySupport);
```

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](../../LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](../../LICENSE-MIT) or http://opensource.org/licenses/MIT)
