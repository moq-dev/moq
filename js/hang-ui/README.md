<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/hang-ui

[![npm version](https://img.shields.io/npm/v/@moq/hang-ui)](https://www.npmjs.com/package/@moq/hang-ui)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

A TypeScript library for interacting with @moq/hang Web Components. Provides methods to control playback and publish sources, as well as status of the connection.

## Installation

```bash
npm add @moq/hang-ui
# or
pnpm add @moq/hang-ui
yarn add @moq/hang-ui
bun add @moq/hang-ui
```

## Web Components

Currently, there are two Web Components provided by @moq/hang-ui:

- `<hang-watch-ui>`
- `<hang-publish-ui>`

Here's how you can use them (see also @moq/hang-demo for a complete example):

```html
<hang-watch-ui>
    <hang-watch url="<MOQ relay URL>" path="<relay path>" muted>
        <canvas style="width: 100%; height: auto; border-radius: 4px; margin: 0 auto;"></canvas>
    </hang-watch>
</hang-watch-ui>
```

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

## Asset Configuration
The hang-ui library loads assets (icons, CSS stylesheets) at runtime from a base path. By default, the library auto-detects the base path by inspecting the script tag that loaded the library. However, you can configure this manually if needed.

### Auto-Detection (Default)
When you include the hang-ui library via a script tag or module, it automatically detects where it was loaded from:

```html
<script type="module" src="/node_modules/@moq/hang-ui/dist/index.js"></script>
```

The library will use `/node_modules/@moq/hang-ui/dist` as the base path for loading assets.

### Manual Configuration with `setBasePath`
If you're serving assets from a different location (CDN, custom build, etc.), you should call `setBasePath()` before any components are initialized:

```typescript
import { setBasePath } from '@moq/hang-ui';

// For npm users with a custom asset location
setBasePath('/assets/hang-ui');

// For CDN users
setBasePath('https://cdn.example.com/hang-ui/v0.1.0');

// For development with copied assets
setBasePath('/public/hang-ui');
```

**Important:** Call `setBasePath()` before initializing any hang-ui components to ensure assets load correctly.

### Asset Structure
When using `setBasePath`, ensure your asset directory contains the required files. The library expects the following structure in the dist folder:

```plaintext
your-base-path/
├── assets/
│   └── icons/
│       └── *.svg (icon files)
└── themes/
    ├── watch/
    │   └── styles.css
    ├── publish/
    │   └── styles.css
    └── *.css (shared theme files)
```

If you installed via npm, these assets are already included in `node_modules/@moq/hang-ui/dist/`. If you're using a CDN or copying the library, make sure to include the entire `dist` folder with all assets.

### Utility Functions
The library exports several utility functions for asset management:

- **`setBasePath(path: string)`**: Override the auto-detected base path for assets
- **`getBasePath(subpath?: string)`**: Get the base path or construct a full asset URL
- **`whenBasePathReady()`**: Wait for the base path to be set (used internally by components)

## Development
### Building
```bash
bun run build
```

This will compile the TypeScript code and copy all assets (icons and themes) to the `dist` folder.

### Testing
```bash
bun run test
```