# MoQ Clock

A TypeScript implementation of a simple clock protocol over MoQ.
This has no practical value; it's just an example of how to support non-media payloads.

This works in the browser or when run using [Bun](https://bun.sh) with a WebTransport polyfill.
In theory it should work for any Javascript runtime that supports WebTransport.

## Usage

The TypeScript implementation mirrors the Rust CLI interface:

```bash
# Publish a clock broadcast
bun ./src/main.ts --url https://cdn.moq.dev/anon --broadcast myclock publish

# Subscribe to a clock broadcast
bun ./src/main.ts --url https://cdn.moq.dev/anon --broadcast myclock subscribe
```

If you're running a relay server locally, use `http://localhost:4443/anon` instead.

## Wire Format

The wire format is identical to the Rust `moq-clock` crate:

1. **Groups**: Each group represents one minute of data
2. **Base Frame**: First frame contains the timestamp base (e.g., "2025-01-31 14:23:")
3. **Second Frames**: Subsequent frames contain individual seconds (e.g., "00", "01", "02", ...)

It's a crude format, but it shows how delta encoding can work.

