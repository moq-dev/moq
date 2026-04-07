# moq-cli

A command-line tool for publishing and subscribing to media over MoQ.
It works with FFmpeg for encoding.

## Install

```bash
cargo install moq-cli
```

### Docker

```bash
docker pull moqdev/moq-cli
```

Multi-arch images (`linux/amd64` and `linux/arm64`) are published to [Docker Hub](https://hub.docker.com/r/moqdev/moq-cli).

## Usage

### Publish a Video File

```bash
moq-cli publish video.mp4 https://relay.example.com/anon/my-stream
```

### Publish from FFmpeg

```bash
ffmpeg -i input.mp4 -f mpegts - | moq-cli publish - https://relay.example.com/anon/my-stream
```
