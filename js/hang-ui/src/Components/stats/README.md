# Stats Component

Real-time statistics display for monitoring media streaming performance (network, video, audio, buffer).

## Usage

```tsx
<Stats 
  context={WatchUIContext}
  getElement={(ctx) => ctx?.hangWatch()}
/>
```

## Props

- **context** - SolidJS context to read from
- **getElement** - Function that extracts the media element from context

The component displays four metrics: network, video, audio, and buffer statistics.