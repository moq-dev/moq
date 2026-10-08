---
title: moq-cli
description: The moq media router, for publishing, playing, converting, and gatewaying
---

# moq-cli

`moq` is a media router. One process connects to a relay (or hosts sessions
itself) and moves media into MoQ from a source, out of MoQ to a sink, or plays
it locally. On macOS or Linux, install it with
`curl -fsSL https://moq.sh | sh`, or use cargo, brew, apt, dnf, winget, or
Docker; see [Install](/setup/install).

## What it does

| Verb | Endpoint | |
| --- | --- | --- |
| `import` | `ts`, `fmp4`, `flv`, `avc3` | Read a container from stdin (usually FFmpeg). |
| `import` | `capture` | Capture a camera, display, window, or app plus a microphone, and encode natively. |
| `import` | `hls <url>` | Pull a remote HLS playlist. |
| `import` | `rtmp`, `srt`, `rtc` | Accept pushes (`--listen`) or pull from a remote (`--connect`). |
| `import` | `archive <url>` | Replay a recording from an object store. |
| `export` | `fmp4`, `mkv`, `ts`, `flv`, `h264`, `h265` | Write a container to stdout. |
| `export` | `hls --listen` | Serve the broadcast as HLS over HTTP. |
| `export` | `rtmp`, `srt`, `rtc` | Serve plays (`--listen`) or push to a remote (`--connect`). |
| `export` | `archive <url>` | Record the broadcast into an object store. |
| `play` | | Decode and play in a native window with sound. |
| `transcode` | | Publish a just-in-time rendition ladder next to a broadcast. |
| `announced` | `[prefix]` | Follow the broadcasts announced on a relay. |
| `fetch` | `<track>` | Write one group of a track to stdout. |
| `auth` | | Generate, sign, and verify relay JWTs. |
| `devices` | | List capture sources and their ids. |

## Grammar

```text
moq <MoQ side> import <source> [options]
moq <MoQ side> export <sink> [options]
moq <MoQ side> play [options]
moq <MoQ side> announced [prefix] [options]
moq <MoQ side> fetch <track> [options]
```

The **MoQ side** goes first and attaches the process to the network:
`--connect <url>` dials a relay (the path is the auth path, `?jwt=`
carries a token), and `--broadcast <name>` names the broadcast. A process can
instead host sessions with `--listen`, or both at once, admitting clients the
way the relay does (see [Authentication](/bin/relay/auth)).
`moq import --help` lists the sources and `moq import rtmp --help` a specific
one; every flag is documented there.

```bash
# Publish a file (remux to MPEG-TS without re-encoding)
ffmpeg -re -i video.mp4 -c copy -f mpegts -pes_payload_size 0 -muxdelay 0 - | \
    moq --connect https://relay.example.com/anon --broadcast my-stream.hang import ts

# Pull it back out
moq --connect https://relay.example.com/anon --broadcast my-stream.hang export ts | ffplay -

# With a token
moq --connect "https://relay.example.com/rooms/1?jwt=$TOKEN" --broadcast alice.hang import ts
```

## Import

The `ts`, `fmp4`, and `flv` imports publish the input's own timestamps, mapped
to the wall time the first frame arrived. A timeline that rewinds, such as a
restarted encoder or a looping file wrapping to the top, ends the import with
an error; run it again to publish anew. A flagged MPEG-TS discontinuity that
jumps forward continues the broadcast and is declared on the exported clock.

MPEG-TS import carries H.264/H.265 and AAC/MP2/AC-3/E-AC-3, and passes SCTE-35
and subtitle PIDs through as tracks. It takes one program: a multi-program
stream is refused until `--program` picks one, or `--program all` publishes
each as its own broadcast (`event.hang` becomes `event/1.hang`,
`event/2.hang`). A damaged packet is dropped on its own PID, so video freezes
until its next keyframe while the other tracks carry on. The importer also
logs feed health (stalled streams, TR 101 290 errors) without changing what it
publishes. FLV covers H.264 + AAC.

## Export

The `fmp4`, `mkv`, `flv`, `h264`, and `h265` exports pick renditions with
`--video-name`, `--audio-name`, `--video-codec`, `--audio-codec`, `--no-video`,
and `--no-audio`. `ts`, `archive`, and the gateways export everything and
refuse these flags.

```bash
moq ... export --no-video fmp4 > audio.mp4
moq ... export --video-name hd --no-audio mkv > hd.mkv
```

fMP4 writes one fragment per publisher group; `--fragment-duration` caps it.
The init segment declares every rendition, so it is written only once each can
be described, and the track set is then fixed: a new rendition or a changed
codec configuration ends the export. Restart it to pick up the change.

MPEG-TS export pads to the source's constant mux rate when the catalog
recorded one, or to `--mux-rate`, so an IRD or groomer receives CBR. Media is
never delayed or dropped to fit.

## Play

```bash
moq --connect https://relay.example.com/anon --broadcast my-stream.hang play
moq ... play --delay 500ms          # fix the delay instead of measuring it
moq ... play --no-video             # audio only
```

Decodes H.264, H.265, and AV1 video with the platform hardware decoder where
available, and Opus, PCM, and AAC-LC audio in software. The opt-in `vpx`
feature adds software VP8 and VP9 through the build host's libvpx. The log
names the decoder each track opened.

Playback starts at the newest group and trails the live edge by `--delay`. The
default, `auto`, sizes the buffer to how unevenly audio arrives, with the same
[algorithm](/concept/audio-jitter) as the browser player. A duration fixes the
delay and is also how long a stalled group is waited on before it is skipped.
Each role follows the catalog, switching rendition when the publisher retires
the one playing. Playback is behind the `play` feature, since it pulls in
windowing and audio-device dependencies:

```bash
cargo install moq-cli --no-default-features --features "iroh,noq,websocket,play"
```

## Capture

```bash
moq --connect https://relay.example.com/anon --broadcast cam.hang import capture
moq ... import capture --display --system-audio          # share a screen with its sound (macOS)
moq ... import capture --window 39193 --no-audio         # one window (macOS, Windows, X11)
moq ... import capture --camera 0 --width 1280 --height 720 --fps 30 --bitrate 3000000 --codec h265
```

Video goes through the platform hardware encoder with an H.264 software
fallback, and audio is Opus; [moq-video](/lib/rs/moq-video) lists the
backends. The camera opens only while someone is watching, and `--bitrate` is a
ceiling that backends with live bitrate control lower to fit the connection.
`moq devices` prints every source id. On Windows, `display:N` is an
enumeration index rather than a stable monitor id, so re-run `moq devices` if a
saved selector picks the wrong screen.

Requires the `capture` feature. On Linux the microphone needs the ALSA
headers, and `--display` and `pipewire:` cameras also need the `pipewire`
feature (links libpipewire).

## Transcode

```bash
moq --connect https://relay.example.com/anon --broadcast cam.hang transcode
moq ... transcode --rung 720:2500000 --rung 360:600000 --encoder nvenc --decoder nvdec
```

Publishes `cam.hang/transcode.hang` whose catalog references the source's
rendition and adds lower rungs that are decoded and encoded only while someone
watches them. On NVIDIA the whole pipeline stays on the GPU. The source is the
largest rendition this host can decode, so a software-only host transcodes
from H.264 rather than a larger H.265 or AV1 one. When the source changes
resolution, rungs are resolved again: one that no longer fits finishes, and
one whose size changed comes back under a new name (`video/360p.2`). Requires
the `transcode` feature.

## Announced

```bash
moq --connect https://relay.example.com/anon announced
moq ... announced room --json
```

Follows the broadcasts announced on a relay over MoQ, with the session's own
auth: the live counterpart of the relay's HTTP `/announced/<prefix>`. Paths are
relative to the `--connect` path. On a terminal it redraws the live list;
piped, it prints `+ path` and `- path` as broadcasts come and go. A name
starting with `.` stays hidden unless `prefix` names it.

## Fetch

```bash
moq --connect https://relay.example.com/anon --broadcast my-stream.hang fetch catalog.json | jq
moq ... fetch video/hd --group 42 --json
```

Writes one group of a track (the newest unless `--group` says otherwise) to
stdout over MoQ: the counterpart of the relay's HTTP `/fetch`. It exits
non-zero when the broadcast or group is not found. [Inspect a
relay](/bin/inspect) walks through `announced` and `fetch` next to their `curl`
equivalents.

## Archive

```bash
# Record a broadcast until it ends
moq --connect https://relay.example.com/anon --broadcast event.hang export archive s3://recordings/event

# Replay it under another name
moq --connect https://relay.example.com/anon --broadcast event-replay.hang import archive s3://recordings/event
```

`export archive` records one broadcast with
[moq-archive](https://docs.rs/moq-archive), cutting each track into 2-10 s
spans at group boundaries. A store that already holds the recording is
continued, unless the source now announces another
[epoch](/concept/moq-lite#publisher-epochs): that is a restart, so the export
fails; record it under a new prefix. `--retention 1h` keeps only the last
hour, a DVR.

`import archive` republishes a recording as live tracks, fetching spans on
request. It ends where the recording ends; `--follow 2s` keeps polling a
recording still being made.

Store URLs are `file:///absolute/path`, `s3://bucket/prefix`,
`gs://bucket/prefix`, or `az://container/prefix`, with credentials from the
usual `AWS_*`, `GOOGLE_*`, and `AZURE_*` environment variables.

## Multiple stages

Separate stages with `--` to bridge several broadcasts, or both directions,
over one connection:

```bash
moq --connect https://relay.example.com/anon \
    import --broadcast event.hang srt --listen 0.0.0.0:9000 \
    -- export --broadcast event.hang hls --listen 0.0.0.0:8080 \
    -- export --broadcast event.hang archive file:///recordings/event
```

## Publisher runs

Each run of `moq` announces a fresh
[publisher epoch](/concept/moq-lite#publisher-epochs), kept across reconnects.
The RTMP, SRT, and WHIP ingests mint one per connection instead, and
`import ts --program all` one per program.

Epochs cross a connection only on moq-lite 07, which is opt-in. There, a
restarted process takes the name at once: subscriptions to the old run end with
`Unroutable`, and a viewer's next subscribe reaches the new run instead of
waiting for its group numbers to catch up. On older versions and moq-transport
the relay sees no epoch and keeps a subscription on the route it first resolved
through, so viewers of a restarted process wait until the old session closes.

On moq-lite 07, two processes that pass the same `--epoch` (a UUIDv7, such as
`uuidgen -7` prints) are one publisher: relays fail over between them
mid-group, so they must produce identical tracks with aligned groups, which no
importer guarantees yet. The ingests that mint their own refuse `--epoch`.

## Cluster

The CLI reads the same `--cluster-*` flags as `moq-relay` and publishes on the
cluster origin, so a `moq` process can join a relay mesh directly. See
[Clustering](/bin/relay/cluster).

```bash
moq --cluster-lan import capture
moq --cluster-lan --cluster-lan-secret /etc/moq/cluster.key import capture
```

`--cluster-lan` advertises this process over mDNS and meshes with every other
MoQ process and `moq-relay` on the LAN, with no other configuration. Without
`--cluster-lan-secret`, anyone who can reach the listener joins, so leave it
unset only on networks you trust. `--cluster-connect` is a MoQ side on its own,
so it needs no `--connect`.

## Auth

```bash
moq auth generate --algorithm ES256 --out private.jwk --public public.jwk
moq auth sign --key private.jwk --root rooms/123 --publish 'alice/**' --subscribe '**' > alice.jwt
moq auth verify --key public.jwk --in alice.jwt
moq auth serve --listen 127.0.0.1:4440 --key-dir keys/ --public-subscribe 'anon/**'
```

`--publish` and `--subscribe` take patterns: `alice` is one broadcast,
`alice/**` is a subtree, `**` is everything under `--root`. `moq auth serve`
answers a relay's auth requests with the same keys (see [Auth
server](/bin/relay/auth#auth-server)). `moq auth sessions` and
`moq auth revalidate` list and re-check sessions on a relay's internal
listener; a re-check asks the auth server again, and its reply is what kicks.
See [Authentication](/bin/relay/auth).

## Retention and latency

`import --max-age` (default 30 s) tells relays how long to keep old groups
fetchable, which the [HLS gateway](/bin/hls) depends on. `export --max-delay`
(default 500 ms) is how far a stalled group may fall behind the live edge
before *this* consumer skips it. Raising the first never delays playback.
`export ts` still spells its budget `--max-age`, and it also bounds how long
the muxer holds a leading track to interleave a lagging one in media-time order.

A stdout export ends with the broadcast. `export ts --linger 10s` waits that
long for a restarted publisher instead, and marks the break in the stream
(PCR discontinuity, PAT/PMT re-sent). Only `ts` can mark a restart, so the
other formats refuse `--linger`.

## Debugging

`RUST_LOG=debug` prints the negotiated version and every subscription.
`moq --connect <url> announced`, or `curl http://relay:4443/announced`, confirms the
relay is reachable and shows what it holds; see [Inspect a relay](/bin/inspect). Connection refused means UDP isn't getting through; certificate
errors on a dev relay want `--connect-tls-insecure` or the `http://`
fingerprint flow.
