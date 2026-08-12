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

`moq-cli` routes one endpoint onto a shared MoQ Origin: `moq <MoQ side> <import|export> <endpoint>`. The MoQ side (before the verb) is either `--connect <url>` (dial a relay) or `--listen <addr>` (self-host). `import` moves media into MoQ, `export` moves it out. The endpoint is a container format (`fmp4`, `ts`, `flv`, ... read from stdin / written to stdout), or a gateway (`hls`, `rtmp`, `srt`, `rtc`). A build with the `play` feature can also render a broadcast locally with `moq <MoQ side> play`.

### Publish to a remote relay

```bash
ffmpeg -i input.mp4 -f mp4 -movflags cmaf - | \
    moq --connect https://relay.example.com --broadcast my-stream.hang import fmp4
```

### Subscribe from a remote relay

```bash
moq --connect https://relay.example.com --broadcast my-stream.hang export fmp4 | \
    ffplay -
```

Play the broadcast in a native window and speaker (requires `--features play` when building):

```bash
moq --connect https://relay.example.com --broadcast my-stream.hang play
```

### Self-host: publish into a local relay

Hosts a MoQ server and publishes a single broadcast read from stdin into it. Useful for local testing without a separate relay process.

```bash
ffmpeg -i input.mp4 -f mp4 -movflags cmaf - | \
    moq --listen '[::]:4443' --listen-tls-generate localhost --broadcast my-stream.hang import fmp4
```

### Self-host: subscribe to an inbound broadcast

Hosts a MoQ server and writes an incoming broadcast's media to stdout. The inverse of the above.

```bash
moq --listen '[::]:4443' --listen-tls-generate localhost --broadcast my-stream.hang export fmp4 | ffplay -
```

### Import formats

- `avc3` raw H.264 Annex-B from stdin
- `fmp4` fragmented MP4 from stdin
