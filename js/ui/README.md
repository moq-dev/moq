<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/ui

[![npm](https://img.shields.io/npm/v/@moq/ui)](https://www.npmjs.com/package/@moq/ui)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Presentation components shared by the [@moq/watch](../watch) and [@moq/publish](../publish) overlays, built on [@moq/signals](../signals).
You only need it directly to build a custom overlay in the same style.

- `controlButton`, `settingsButton`, `icon`: overlay control buttons and SVG glyphs.
- `graph`: a rolling sparkline for stats panels.
- `Support`: the base of `<moq-watch-support>` and `<moq-publish-support>`, a banner and details table for a browser support report.

It holds no styles or capability detection: each overlay styles these elements and detects its own support.

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
