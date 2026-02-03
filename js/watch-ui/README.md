<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/watch-ui

[![npm version](https://img.shields.io/npm/v/@moq/watch-ui)](https://www.npmjs.com/package/@moq/watch-ui)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

A TypeScript library providing UI components for @moq/watch. Includes playback controls, quality selection, and buffering indicators.

## Installation

```bash
npm add @moq/watch-ui
# or
bun add @moq/watch-ui
```

## Usage

```html
<hang-watch-ui>
    <hang-watch url="<MOQ relay URL>" path="<relay path>" muted>
        <canvas style="width: 100%; height: auto; border-radius: 4px; margin: 0 auto;"></canvas>
    </hang-watch>
</hang-watch-ui>
```

## Features

- **WatchControls**: Main control panel for the video player
- **PlayPauseButton**: Play/pause toggle
- **VolumeSlider**: Audio volume control
- **LatencySlider**: Adjust playback latency
- **QualitySelector**: Switch between quality levels
- **FullscreenButton**: Toggle fullscreen mode
- **BufferingIndicator**: Visual feedback during buffering
- **StatsButton**: Toggle statistics panel

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
