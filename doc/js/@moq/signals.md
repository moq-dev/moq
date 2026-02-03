---
title: "@moq/signals"
description: Reactive signals library
---

# @moq/signals

Reactive signals library used by `@moq/hang` for state management.

## Overview

`@moq/signals` provides:

- Reactive primitives for state management
- Framework adapters (React, Solid, Vue)
- Used internally by `@moq/hang`

## Installation

```bash
bun add @moq/signals
# or
npm add @moq/signals
```

## Basic Usage

### Creating Signals

```typescript
import { signal } from "@moq/signals";

const count = signal(0);

// Get current value
console.log(count.get()); // 0

// Set value
count.set(1);

// Subscribe to changes
const unsubscribe = count.subscribe((value) => {
    console.log("Count changed:", value);
});

// Cleanup
unsubscribe();
```

### Computed Signals

```typescript
import { signal, computed } from "@moq/signals";

const firstName = signal("John");
const lastName = signal("Doe");

const fullName = computed(() => {
    return `${firstName.get()} ${lastName.get()}`;
});

console.log(fullName.get()); // "John Doe"

firstName.set("Jane");
console.log(fullName.get()); // "Jane Doe"
```

### Effects

```typescript
import { signal, effect } from "@moq/signals";

const count = signal(0);

const cleanup = effect(() => {
    console.log("Count is:", count.get());
});

count.set(1); // Logs: "Count is: 1"
count.set(2); // Logs: "Count is: 2"

cleanup();
```

## Framework Adapters

### React

```typescript
import { signal } from "@moq/signals";
import { react } from "@moq/signals/react";
import { useEffect, useState } from "react";

const count = signal(0);

function Counter() {
    const reactiveCount = react(count);

    return (
        <div>
            Count: {reactiveCount()}
            <button onClick={() => count.set(count.get() + 1)}>
                Increment
            </button>
        </div>
    );
}
```

### SolidJS

```typescript
import { signal } from "@moq/signals";
import { solid } from "@moq/signals/solid";

const count = signal(0);

function Counter() {
    const solidCount = solid(count);

    return (
        <div>
            Count: {solidCount()}
            <button onClick={() => count.set(count.get() + 1)}>
                Increment
            </button>
        </div>
    );
}
```

### Vue

```typescript
import { signal } from "@moq/signals";
import { vue } from "@moq/signals/vue";

const count = signal(0);

// In Vue component
const vueCount = vue(count);
```

## Usage with @moq/hang

All `@moq/hang` properties are signals:

```typescript
import "@moq/hang/watch/element";
import { react } from "@moq/signals/react";

const watch = document.querySelector("hang-watch") as HangWatch;

// Convert to React-compatible signal
const volume = react(watch.volume);
const paused = react(watch.paused);

function Controls() {
    return (
        <div>
            <span>Volume: {volume()}</span>
            <span>Paused: {paused() ? "Yes" : "No"}</span>

            <input
                type="range"
                min="0"
                max="1"
                step="0.1"
                value={volume()}
                onChange={(e) => watch.volume.set(Number(e.target.value))}
            />
        </div>
    );
}
```

## API Reference

### signal(initialValue)

Create a new reactive signal.

```typescript
const s = signal<T>(initialValue: T): Signal<T>
```

### computed(fn)

Create a computed signal that derives from other signals.

```typescript
const c = computed<T>(fn: () => T): ReadonlySignal<T>
```

### effect(fn)

Run a side effect when signals change.

```typescript
const cleanup = effect(fn: () => void): () => void
```

### batch(fn)

Batch multiple signal updates.

```typescript
batch(() => {
    signal1.set(1);
    signal2.set(2);
    // Subscribers notified once at the end
});
```

## Next Steps

- Use with [@moq/hang](/js/@moq/hang/)
- View [code examples](https://github.com/moq-dev/moq/tree/main/js)
- Learn about [Web Components](/js/env/web)
