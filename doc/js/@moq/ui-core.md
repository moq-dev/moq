---
title: "@moq/ui-core"
description: Shared UI primitives for MoQ components
---

# @moq/ui-core

[![npm](https://img.shields.io/npm/v/@moq/ui-core)](https://www.npmjs.com/package/@moq/ui-core)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Shared UI primitives used by `@moq/watch/ui` and `@moq/publish/ui`. Includes buttons, icons, stats panels, and a CSS theme.

## Installation

```bash
bun add @moq/ui-core
# or
npm add @moq/ui-core
```

## Components

- **Button** — Styled button component
- **Icon** — SVG icon library (play, pause, mic, camera, etc.)
- **Stats** — Network statistics panel
- **CSS Theme** — Shared CSS variables and base styles

## Usage

This package is primarily consumed internally by `@moq/watch/ui` and `@moq/publish/ui`. You typically don't need to install it directly unless building custom UI on top of MoQ.

```typescript
import { Button, Icon, Stats } from "@moq/ui-core";
```

## Related Packages

- **[@moq/watch](/js/@moq/watch)** — Subscribe to and render MoQ broadcasts
- **[@moq/publish](/js/@moq/publish)** — Publish media to MoQ broadcasts
- **[@moq/hang](/js/@moq/hang/)** — Core media library
