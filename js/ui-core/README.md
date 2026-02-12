<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/ui-core

[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Shared UI components for [Media over QUIC](https://moq.dev/) (MoQ) packages.

`@moq/ui-core` provides reusable, accessible UI primitives used by `@moq/watch/ui` and `@moq/publish/ui`, built with [SolidJS](https://www.solidjs.com/).

## Components

### Button
A styled, accessible button component with hover/active states and disabled support.

### Icon
SVG icon library including media controls (play, pause, volume, fullscreen, etc.), device indicators (camera, microphone, screen), and stats icons (network, video, audio, buffer).

### Stats
Real-time statistics panel for monitoring media streaming performance. Displays network, video, audio, and buffer metrics via a provider pattern.

## CSS

Shared stylesheets are available as CSS imports:

- `@moq/ui-core/variables.css` — Theme variables (colors, spacing, border-radius)
- `@moq/ui-core/flex.css` — Flexbox utility classes
- `@moq/ui-core/button/button.css` — Button component styles
- `@moq/ui-core/stats/styles/index.css` — Stats panel styles

## License

Licensed under either:

-   Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
-   MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
