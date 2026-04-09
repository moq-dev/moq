<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/boy

[![npm](https://img.shields.io/npm/v/@moq/boy)](https://www.npmjs.com/package/@moq/boy)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Crowd-controlled Game Boy streaming via [Media over QUIC](https://moq.dev/) (MoQ). Provides web components to view and interact with Game Boy emulator sessions streamed over a MoQ relay.

## Installation

```bash
bun add @moq/boy
# or
npm add @moq/boy
```

## Web Components

### `<moq-boy>` — Game Grid

Shows a grid of all active Game Boy sessions. Click a game to expand and play.

```html
<script type="module">
    import "@moq/boy/element";
</script>

<moq-boy url="https://cdn.moq.dev/anon"></moq-boy>
```

#### Attributes

| Attribute | Type   | Description      |
|-----------|--------|------------------|
| `url`     | string | Relay server URL |

## JavaScript API

For programmatic use:

```typescript
import { GameCard } from "@moq/boy";

const card = new GameCard({
    sessionId: "opossum",
    connection,
    expanded,
    root: document.body,
});

document.body.appendChild(card.el);
```

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](../../LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](../../LICENSE-MIT) or http://opensource.org/licenses/MIT)
