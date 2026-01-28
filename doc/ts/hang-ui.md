---
title: "@moq/hang-ui"
description: SolidJS UI components for MoQ
---

# @moq/hang-ui

[![npm](https://img.shields.io/npm/v/@moq/hang-ui)](https://www.npmjs.com/package/@moq/hang-ui)

SolidJS UI components that work with `@moq/hang` for building video streaming interfaces.

## Overview

`@moq/hang-ui` provides ready-made SolidJS components for:

- Video playback controls
- Quality selection
- Volume controls
- Publishing controls
- Chat interface
- Network statistics

## Installation

```bash
bun add @moq/hang-ui
# or
npm add @moq/hang-ui
pnpm add @moq/hang-ui
```

## Quick Start

```tsx
import { HangWatch, HangPublish } from "@moq/hang-ui";

function App() {
    return (
        <div>
            <HangPublish
                url="https://relay.example.com/anon"
                path="room/alice"
                audio video controls
            />

            <HangWatch
                url="https://relay.example.com/anon"
                path="room/alice"
                controls
            />
        </div>
    );
}
```

## Components

### HangWatch

Video player with controls:

```tsx
import { HangWatch } from "@moq/hang-ui/watch";

<HangWatch
    url="https://relay.example.com/anon"
    path="stream"
    controls
    volume={0.8}
/>
```

### HangPublish

Publisher with preview:

```tsx
import { HangPublish } from "@moq/hang-ui/publish";

<HangPublish
    url="https://relay.example.com/anon"
    path="my-stream"
    device="camera"
    audio video controls
/>
```

### HangMeet

Video conferencing:

```tsx
import { HangMeet } from "@moq/hang-ui/meet";

<HangMeet
    url="https://relay.example.com/anon"
    path="room123"
    audio video controls
/>
```

### QualitySelector

Quality selection dropdown:

```tsx
import { QualitySelector } from "@moq/hang-ui/controls";

<QualitySelector watch={watchInstance} />
```

### VolumeControl

Volume slider:

```tsx
import { VolumeControl } from "@moq/hang-ui/controls";

<VolumeControl watch={watchInstance} />
```

### PlayPauseButton

Play/pause toggle:

```tsx
import { PlayPauseButton } from "@moq/hang-ui/controls";

<PlayPauseButton watch={watchInstance} />
```

## Customization

Components accept standard SolidJS props for styling:

```tsx
<HangWatch
    url="..."
    path="..."
    class="my-video-player"
    style={{ borderRadius: "8px" }}
/>
```

## Integration with @moq/hang

Access the underlying hang instance:

```tsx
import { HangWatch } from "@moq/hang-ui/watch";
import { createSignal, onMount } from "solid-js";

function App() {
    let watchRef;

    onMount(() => {
        // Access @moq/hang instance
        const hangInstance = watchRef.hang;

        // Subscribe to catalog
        hangInstance.catalog.subscribe((catalog) => {
            console.log("Tracks:", catalog?.tracks);
        });
    });

    return (
        <HangWatch
            ref={watchRef}
            url="https://relay.example.com/anon"
            path="stream"
        />
    );
}
```

## Theming

Components use CSS custom properties for theming:

```css
:root {
    --hang-ui-primary: #007bff;
    --hang-ui-background: #1a1a1a;
    --hang-ui-text: #ffffff;
    --hang-ui-border-radius: 8px;
}
```

## Next Steps

- Learn about [@moq/hang](/ts/hang/)
- View [code examples](/ts/examples)
- Learn about [Web Components](/web/)
