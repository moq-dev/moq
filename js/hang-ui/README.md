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
		<video style="width: 100%; height: auto; border-radius: 4px; margin: 0 auto;" muted autoplay></video>
	</hang-publish>
</hang-publish-ui>
```

## Project Structure
The `@moq/hang-ui` package is organized into modular components and utilities:

```text
src/
├── publish/             # Publishing UI components
│   ├── components/      # UI controls for publishing
│   ├── hooks/           # Custom Solid hooks for publish UI
│   ├── styles/          # CSS styles for publish UI
│   ├── context.tsx      # Context provider for publish state
│   ├── element.tsx      # Main publish UI component
│   └── index.tsx        # Entry point for publish UI
│
├── watch/               # Watching/playback UI components
│   ├── components/      # UI controls for watching
│   ├── hooks/           # Custom Solid hooks for watch UI
│   ├── styles/          # CSS styles for watch UI
│   ├── context.tsx      # Context provider for watch state
│   ├── element.tsx      # Main watch UI component
│   └── index.tsx        # Entry point for watch UI
│
└── shared/              # Shared components and utilities
    ├── components/      # Reusable UI components
	│   ├── button/      # Button component
	│   ├── icon/        # Icon component
	│   └── stats/       # Statistics and monitoring components
    ├── flex.css         # Flexbox utilities
    └── variables.css    # CSS variables and theme

```

### Module Overview

#### **publish/**
Contains all UI components related to media publishing. It provides controls for selecting media sources (camera, screen, microphone, file) and managing the publishing state.

- **MediaSourceSelector**: Allows users to choose their media source
- **PublishControls**: Main control panel for publishing
- **Source buttons**: Individual buttons for camera, screen, microphone, file, and "nothing" sources
- **PublishStatusIndicator**: Displays connection and publishing status

#### **watch/**
Implements the video player UI with controls for watching live streams. Includes playback controls, quality selection, and buffering indicators.

- **WatchControls**: Main control panel for the video player
- **PlayPauseButton**: Play/pause toggle
- **VolumeSlider**: Audio volume control
- **LatencySlider**: Adjust playback latency
- **QualitySelector**: Switch between quality levels
- **FullscreenButton**: Toggle fullscreen mode
- **BufferingIndicator**: Visual feedback during buffering
- **StatsButton**: Toggle statistics panel

#### **shared/**
Common components and utilities used across the package.

- **Button**: Reusable button component with consistent styling
- **Icon**: Icon wrapper component
- **Stats**: Provides real-time statistics monitoring for both audio and video streams. Uses a provider pattern to collect and display metrics.
- **CSS utilities**: Shared styles, variables, and flexbox utilities

---

## Build System & Code Generation

This package uses Custom Elements Manifest (CEM) to automatically generate framework-specific wrappers.

### How It Works

1. **CEM analysis** (`cem analyze`) scans Web Components and creates `custom-elements.json`
2. **JSDoc enhancement** extracts `@tag`, `@summary`, `@example` from source files
3. **Wrapper generation** creates typed framework components in `src/wrappers/<framework>/`

### Available Scripts

```bash
bun run prebuild   # Generate CEM + framework wrappers
bun run build      # Build package with Vite + TypeScript declarations
```

The code generator lives in `../scripts/element-wrappers/`:

### React Wrappers

Auto-generated from CEM, exported from `@moq/hang-ui/react`:

```tsx
import { HangWatchUI, HangPublishUI } from '@moq/hang-ui/react';

<HangWatchUI>
  <hang-watch url="..." path="...">
    <canvas />
  </hang-watch>
</HangWatchUI>
```

### Generating Wrappers for Other Frameworks

To add Vue, Angular, or other frameworks:

#### 1. Create Generator

Create `../scripts/element-wrappers/generators/vue.ts`:

```ts
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { extractCustomElements, loadManifest, tagNameToComponentName } from "../utils/manifest";
import { formatCode, generateJSDoc } from "../utils/codegen";

function generateVueComponent(element) {
  const name = tagNameToComponentName(element.tagName);
  const jsDoc = generateJSDoc(
    element.summary, element.description,
    element.slots, element.events,
    element.attributes, element.properties,
    element.examples
  );

  return `${jsDoc}
export const ${name} = defineComponent({
  name: '${name}',
  template: '<${element.tagName}><slot /></${element.tagName}>',
});`;
}

export function generateVueWrappers(basePath = process.cwd()) {
  console.log("\n🔧 Generating Vue wrappers...");
  
  const manifest = loadManifest(basePath);
  const elements = extractCustomElements(manifest);
  if (elements.length === 0) return;

  const output = `import { defineComponent } from 'vue';\n\n${
    elements.map(generateVueComponent).join("\n")
  }`;

  const outputDir = join(basePath, "src", "wrappers", "vue");
  mkdirSync(outputDir, { recursive: true });
  writeFileSync(join(outputDir, "index.ts"), formatCode(output));

  console.log(`✅ Generated ${elements.length} Vue wrapper(s)`);
}
```

#### 2. Register Generator

In `../scripts/element-wrappers/index.ts`:

```ts
import { generateVueWrappers } from "./generators/vue";

const generators = [
  { name: "React", fn: generateReactWrappers, enabled: true },
  { name: "Vue", fn: generateVueWrappers, enabled: true },
];
```

#### 3. Update Package Configuration

**package.json** - Add export:
```json
{
  "exports": {
    "./vue": {
      "types": "./wrappers/vue/index.d.ts",
      "default": "./src/wrappers/vue/index.ts"
    }
  }
}
```

**vite.config.ts** - Add build entry:
```ts
entry: {
  "wrappers/vue/index": resolve(__dirname, "src/wrappers/vue/index.ts"),
}
```

#### 4. Run

```bash
bun run prebuild  # Generates src/wrappers/vue/index.ts
bun run build     # Compiles to dist/
```

For more details, see [`../scripts/element-wrappers/README.md`](../scripts/element-wrappers/README.md).

---
