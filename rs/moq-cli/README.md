# moq-cli

A command-line tool for publishing and subscribing to media over MoQ.
It works with FFmpeg for encoding and decoding.

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

`moq-cli` reads media from stdin (or writes media to stdout) and exchanges it with a MoQ relay. Pick a subcommand based on whether you want to publish or subscribe, and whether your relay is local or remote.

### Publish to a remote relay

```bash
ffmpeg -i input.mp4 -f mp4 -movflags cmaf - | \
    moq-cli publish --url https://relay.example.com --broadcast my-stream fmp4
```

### Subscribe from a remote relay

```bash
moq-cli subscribe --url https://relay.example.com --broadcast my-stream --format fmp4 | \
    ffplay -
```

### Self-host: publish into a local relay (`serve`)

Runs a relay and publishes a single broadcast read from stdin into it. Useful for local testing without a separate relay process.

```bash
ffmpeg -i input.mp4 -f mp4 -movflags cmaf - | \
    moq-cli serve --broadcast my-stream fmp4
```

### Self-host: subscribe to an inbound broadcast (`accept`)

Runs a relay and writes the first incoming broadcast's media to stdout. The inverse of `serve`.

```bash
moq-cli accept --broadcast my-stream --format fmp4 | ffplay -
```

### Input formats (`publish` / `serve`)

- `avc3` raw H.264 Annex-B from stdin
- `fmp4` fragmented MP4 from stdin
