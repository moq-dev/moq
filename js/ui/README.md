<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/ui

[![npm](https://img.shields.io/npm/v/@moq/ui)](https://www.npmjs.com/package/@moq/ui)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Presentation components shared by the [@moq/watch](../watch) and [@moq/publish](../publish) overlays, built on [@moq/signals](../signals).
You only need it directly to build a custom overlay in the same style.

- `controlButton`, `settingsButton`, `fullscreenButton`, `icon`: overlay control buttons and SVG glyphs.
- `createFullscreen`: cross-browser fullscreen with an iPhone pseudo-fullscreen fallback, its state a signal.
- `statCard`, `statLine`, `graph`: stats panel cards, key/value rows, and a rolling sparkline.
- `formatBitrate`, `formatMillis`, `formatHz`, `formatFps`: human-readable stats values.
- `Support`: the base of `<moq-watch-support>` and `<moq-publish-support>`, a banner and details table for a browser support report.

It holds no capability detection and publishes no styles: each overlay styles these elements, including the `player--pseudo-fullscreen` class, and detects its own support.
Inside this repository, `@moq/watch` and `@moq/publish` inline the shared stats card and sparkline styles from `src/stats.css` at build time; that file is not part of the npm package.

```bash
bun add @moq/ui
# or
npm add @moq/ui
```

```ts
import { Effect } from "@moq/signals";
import { graph } from "@moq/ui";

const effect = new Effect();
const bitrate = graph(effect, "Bitrate", { color: "#a855f7" });
document.body.append(bitrate.el);
bitrate.push(2_500_000);
```

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](../../LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](../../LICENSE-MIT) or http://opensource.org/licenses/MIT)
