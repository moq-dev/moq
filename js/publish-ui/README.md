<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/publish-ui

[![npm version](https://img.shields.io/npm/v/@moq/publish-ui)](https://www.npmjs.com/package/@moq/publish-ui)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

A TypeScript library providing UI components for @moq/publish. Includes controls for selecting media sources and managing publishing state.

## Installation

```bash
npm add @moq/publish-ui
# or
bun add @moq/publish-ui
```

## Usage

```html
<hang-publish-ui>
    <hang-publish url="<MOQ relay URL>" path="<relay path>">
        <video
            style="width: 100%; height: auto; border-radius: 4px; margin: 0 auto;"
            muted
            autoplay
        ></video>
    </hang-publish>
</hang-publish-ui>
```

## Features

- **MediaSourceSelector**: Allows users to choose their media source
- **PublishControls**: Main control panel for publishing
- **Source buttons**: Individual buttons for camera, screen, microphone, file, and "nothing" sources
- **PublishStatusIndicator**: Displays connection and publishing status

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
