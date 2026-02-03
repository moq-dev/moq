---
title: FFmpeg / moq-cli
description: Command-line tools for MoQ media
---

# FFmpeg / moq-cli

`moq-cli` is a command-line tool for publishing media to MoQ relays. It works with FFmpeg for encoding.

## Installation

### Using Cargo

```bash
cargo install moq-cli
```

### Using Nix

```bash
nix build github:moq-dev/moq#moq
```

### From Source

```bash
git clone https://github.com/moq-dev/moq
cd moq
cargo build --release --bin moq
```

The binary will be in `target/release/moq`.

## Basic Usage

### Publish a Video File

```bash
moq publish video.mp4 https://relay.example.com/anon/my-stream
```

### Publish from FFmpeg

Pipe FFmpeg output directly to moq:

```bash
ffmpeg -i input.mp4 -f mpegts - | moq publish - https://relay.example.com/anon/my-stream
```

### Publish a Webcam

```bash
# macOS
ffmpeg -f avfoundation -i "0:0" -f mpegts - | moq publish - https://relay.example.com/anon/webcam

# Linux
ffmpeg -f v4l2 -i /dev/video0 -f mpegts - | moq publish - https://relay.example.com/anon/webcam
```

### Publish Screen

```bash
# macOS
ffmpeg -f avfoundation -i "1:" -f mpegts - | moq publish - https://relay.example.com/anon/screen

# Linux (X11)
ffmpeg -f x11grab -i :0.0 -f mpegts - | moq publish - https://relay.example.com/anon/screen
```

## Encoding Options

### Custom Video Settings

```bash
ffmpeg -i input.mp4 \
    -c:v libx264 -preset ultrafast -tune zerolatency \
    -b:v 2500k -maxrate 2500k -bufsize 5000k \
    -c:a aac -b:a 128k \
    -f mpegts - | moq publish - https://relay.example.com/anon/stream
```

### Low Latency Settings

```bash
ffmpeg -i input.mp4 \
    -c:v libx264 -preset ultrafast -tune zerolatency \
    -g 30 -keyint_min 30 \
    -c:a aac \
    -f mpegts - | moq publish - https://relay.example.com/anon/stream
```

### H.265/HEVC

```bash
ffmpeg -i input.mp4 \
    -c:v libx265 -preset ultrafast \
    -c:a aac \
    -f mpegts - | moq publish - https://relay.example.com/anon/stream
```

## Authentication

Pass a JWT token via the URL:

```bash
moq publish video.mp4 "https://relay.example.com/room/123?jwt=<token>"
```

See [Authentication](/app/relay/auth) for token generation.

## Test Videos

The repository includes helper commands for test content:

```bash
# Publish Big Buck Bunny
just pub bbb https://relay.example.com/anon

# Publish Tears of Steel
just pub tos https://relay.example.com/anon
```

## Clock Synchronization

Publish and subscribe to clock broadcasts for testing:

```bash
# Publish a clock
just clock publish https://relay.example.com/anon

# Subscribe to a clock
just clock subscribe https://relay.example.com/anon
```

## Debugging

### Verbose Output

```bash
RUST_LOG=debug moq publish video.mp4 https://relay.example.com/anon/stream
```

### Check Connection

```bash
# Verify you can connect to the relay
curl http://relay.example.com:4443/announced/
```

## Common Issues

### "Connection refused"
- Ensure the relay is running
- Check firewall allows UDP traffic
- Verify the URL is correct

### "Invalid certificate"
- The relay needs a valid TLS certificate
- For development, use the fingerprint method
- See [TLS Setup](/app/relay/#tls-setup)

### "Permission denied"
- Check your JWT token is valid
- Verify the token allows publishing to that path
- See [Authentication](/app/relay/auth)

## Next Steps

- Deploy a [relay server](/app/relay/)
- Use [Web Components](/js/env/web) for playback
- Try the [Rust libraries](/rs/) for custom apps
