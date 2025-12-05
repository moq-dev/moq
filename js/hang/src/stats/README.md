# hang-stats

A real-time statistics web component for monitoring media streaming performance. The `hang-stats` component displays live metrics for audio, video, network, and buffer streams, providing developers and users with instant visibility into stream quality, network conditions, and buffer status.

Built as a generic, reusable web component that automatically detects and monitors any parent element exposing audio and video stream sources through reactive signal-based APIs.

## Features

- **Real-time Metrics** - Live updates of network, video, audio, and buffer statistics with automatic refresh on stream changes
- **Zero Configuration** - Drop-in web component with automatic parent stream detection; no setup required
- **UI** - Clean, accessible UI with clear metric visualization and expandable/collapsible panel
- **Truly Agnostic** - Generic type system works with any parent element providing audio/video streams through signal-based APIs
- **Reactive Architecture** - Built with SolidJS for efficient, fine-grained reactive updates and minimal re-renders
- **Generic Types** - Uses generic interfaces and type guards for stream source detection, not tied to specific implementations
- **Extensible Handler System** - Simple handler interface for adding new metric types or customizing calculations
- **Lightweight** - Minimal dependencies, only relies on reactive signals for parent stream detection

## Displayed Metrics

The stats component tracks four essential metrics for media streaming:

| Metric | Tracks | Example Display |
|--------|--------|-----------------|
| **Network** | Connection type, effective bandwidth, latency, and save-data preferences | `4G • 10 Mbps • 45ms`   |
| **Video** | Resolution, frame rate, and display properties                             | `1920x1080 @ 60 FPS`    |
| **Audio** | Channel configuration, codec, and bitrate                                  | `2ch stereo • 128 kbps` |
| **Buffer** | Stream buffer fill percentage and latency value                           | `85% 100ms`             |

Each metric is continuously updated as stream properties change, with intelligent formatting that shows "N/A" when data is unavailable.

## Usage

### Installation

The component is automatically exported as part of the hang library:

```typescript
import "@kixelated/hang/stats";
```

### Basic Usage

Simply nest the component in any HTML element:

```html
<hang-stats></hang-stats>
```

The component will automatically:
1. Search parent DOM tree for elements with active stream sources
2. Extract audio and video streams from discovered parent
3. Initialize metric handlers for each available stream type
4. Display real-time statistics in a collapsible panel
5. Clean up subscriptions when removed from the DOM

### Typical Integration

```typescript
import "@kixelated/hang/stats";

// In your component/page
const statsElement = document.createElement("hang-stats");
containerElement.appendChild(statsElement);

// The component automatically finds parent streams and displays metrics
```

## How It Works

### Stream Discovery Process

The `hang-stats` component uses an intelligent discovery mechanism to find and connect to parent stream sources:

1. **DOM Tree Traversal** - Walks up the DOM hierarchy from the component to parent elements
2. **Stream Extraction** - Checks each parent for active stream containers with audio/video sources
3. **Signal Detection** - Identifies parent elements that expose audio and video through reactive signal APIs
4. **Dynamic Connection** - Establishes subscriptions to stream signals and responds to all changes

The discovery is **completely generic** and works with any parent structure that exposes:
- An `audio` property with reactive signal properties
- A `video` property with reactive signal properties
- Properties that follow the signal pattern: `peek()` and optional `subscribe(callback)`

### Component Architecture

The component is built from several specialized parts:

**UI Components:**
- **StatsWrapper** - Root container managing overall component visibility and layout
- **StatsPanel** - Main display panel showing all metrics in a organized grid
- **StatsItem** - Individual metric cell displaying icon, label, and formatted value
- **Button** - Toggle button to show/hide the stats panel from view

**Metric Handlers:**
- **NetworkHandler** - Monitors network connection type (WiFi, 4G, etc.), effective bandwidth, round-trip latency, and data-saver mode status
- **VideoHandler** - Tracks video resolution (width × height), frame rate (FPS), and display properties
- **AudioHandler** - Monitors audio channel count, codec configuration, and bitrate in kilobits per second
- **BufferHandler** - Calculates buffer fill percentage, tracks sync status (ready vs waiting), and displays buffer state

Each handler is independent and follows the `IStatsHandler` interface, making it easy to add custom metric types.

### Reactive Signal System

The component uses reactive signals for all data flow:

```typescript
// Signal interface - minimal contract for reactive values
interface Signal<T> {
    peek(): T | undefined;                             // Get current value synchronously
    subscribe?: (callback: () => void) => () => void;  // Optional subscription
}

// Audio stream format
interface AudioSource {
    active: Signal<string>;           // Which audio track is active
    config: Signal<AudioConfig>;      // Audio configuration (channels, codec)
    bitrate: Signal<number>;          // Current bitrate in kbps
}

// Video stream format
interface VideoSource {
    display: Signal<{ width: number; height: number }>;  // Resolution
    fps: Signal<number>;                                 // Frame rate
    syncStatus: Signal<...>;                             // Sync state
    bufferStatus: Signal<...>;                           // Buffer state
    latency: Signal<number>;                             // Latency in milliseconds
}
```

When a parent element exposes these signals, hang-stats automatically connects and monitors them.

### Handler Lifecycle

Each metric handler follows this lifecycle:

1. **Construction** - Handler is instantiated with generic `HandlerProps` (audio/video sources)
2. **Setup** - `setup(context)` called with display context, handler subscribes to relevant signals
3. **Updates** - On any signal change, handler recalculates and formats the metric
4. **Display** - Formatted value is passed to `context.setDisplayData()`
5. **Cleanup** - `cleanup()` called when component is removed, all subscriptions are unsubscribed

This architecture ensures:
- Subscriptions are properly cleaned up to prevent memory leaks
- Updates only occur when underlying data changes (reactive)
- Each handler is independent and can be extended or replaced

## Architecture & Design

### Type System

The stats component uses **fully generic types** for complete parent independence:

```typescript
// Generic handler props - works with any stream source
interface HandlerProps {
    audio?: Signal<AudioSource>;     // Optional audio stream
    video?: Signal<VideoSource>;     // Optional video stream
}

// Handler interface
interface IStatsHandler {
    setup(context: HandlerContext): void;
    cleanup(): void;
}

// Display context for handlers
interface HandlerContext {
    setDisplayData(data: string): void;     // Set metric display text
    setFps?(fps: number | null): void;      // Optional FPS callback
}
```

**Key Design Principles:**
- No imports from specific parent components
- No type coupling to particular implementations
- Handlers work with any object that exposes signal-like APIs
- Type guards (`isValidStreamSource()`) validate parent structure at runtime

### Performance Characteristics

The component is optimized for minimal overhead:

- **Lazy Rendering** - Stats panel starts hidden, only renders when toggled visible
- **Reactive Updates** - Uses fine-grained reactivity; only affected components re-render on data changes
- **Efficient Subscriptions** - Handlers only subscribe to signals they need; unsubscribe on cleanup
- **No Polling** - Event-driven architecture using signal subscriptions, not timers or interval polling
- **Direct DOM Updates** - Built with SolidJS which compiles to direct DOM manipulations without a virtual DOM

Typical memory footprint: ~50-100KB including all handlers and UI components.

## Development

### Adding a New Metric Handler

To add a new metric type:

1. Create handler file in `handlers/`
2. Extend `BaseHandler` and implement `IStatsHandler`
3. Subscribe to relevant signals in `setup()`
4. Calculate and display formatted value in `updateDisplayData()`
5. Register in `handlers/registry.ts`

Example:

```typescript
export class CustomHandler extends BaseHandler {
    setup(context: HandlerContext): void {
        // Subscribe to signals and store context
        this.subscribe(this.props.video?.fps, () => this.updateDisplayData());
    }

    private updateDisplayData(): void {
        const fps = this.peekSignal<number>(this.props.video?.fps);
        this.context?.setDisplayData(`FPS: ${fps ?? "N/A"}`);
    }
}
```