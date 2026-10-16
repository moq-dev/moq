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
way the relay does (see [Authentication](/bin/relay/auth)). Listening imports and
exports share the relay's shutdown behavior: SIGINT or SIGTERM sends accepted
sessions a reconnect-to-me GOAWAY and waits up to `--drain-timeout` (default
`10s`), exiting once they have all left. Sessions on versions without GOAWAY,
such as `moq-lite-03`, get none and are closed when the window ends. A second
signal exits immediately; `--drain-timeout 0s` closes sessions immediately.
Pending handshakes and admission requests share the same drain window, and an
open stdin pipe does not keep the process alive afterward.

Add `--stats-enabled` before the first stage to publish accepted-session and
traffic counters with the relay's `--stats-*` settings. An export-only listener
publishes nothing to its sessions, so they cannot read these stats. These
listener settings require a MoQ listener. Grants still scope which broadcasts
each session can publish or subscribe to, including stats; see the relay's
[statistics configuration](/bin/relay/config#stats).

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
and subtitle PIDs through as tracks. SCTE-35 cues wait for a media timestamp;
input ending or declaring a clock break before that timestamp is refused.
PES and untimed section staging share a bounded budget, so a source that never
supplies a PES boundary or clock fails rather than retaining bytes indefinitely.
It takes one program: a multi-program
stream is refused until `--program` picks one, or `--program all` publishes
each as its own broadcast (`event.hang` becomes `event/1.hang`,
`event/2.hang`). A damaged packet is dropped on its own PID, so video freezes
until its next keyframe while the other tracks carry on. The importer also
logs feed health (stalled streams, TR 101 290 errors) without changing what it
publishes. `--passthrough` publishes the multiplex whole instead, every packet
in order on one [`m2ts`](/concept/hang#transport-streams) track with every
program aboard, for what demultiplexing cannot carry, such as a scrambled
service; `export ts --passthrough` writes it back out. FLV covers H.264 + AAC.

## Export

The `fmp4`, `mkv`, `flv`, `h264`, and `h265` exports pick renditions with
`--video-name`, `--audio-name`, `--video-codec`, `--audio-codec`, `--no-video`,
and `--no-audio`. `ts`, `archive`, and the gateways export everything and
refuse these flags.

```bash
moq ... export --no-video fmp4 > audio.mp4
moq ... export --video-name hd --no-audio mkv > hd.mkv
```

Every export skips a disabled rendition (catalog `enabled: false`). `h264` and
`h265` move to an enabled sibling when theirs is disabled, and back at its next
keyframe when it re-enables. The muxed exports declare only enabled renditions
in their header; one disabled afterwards keeps its track, idle until it
re-enables.

fMP4 writes one fragment per publisher group; `--fragment-duration` caps it.
The init segment declares every enabled rendition, so it is written only once each can
be described, and the track set is then fixed: a new rendition or a changed
codec configuration ends the export. Restart it to pick up the change.
Annex-B H.264 and H.265 whose catalog codec string and dimensions fix the sample
entry are described at once as `avc3` or `hev1`, with SPS, PPS, and VPS kept in
the samples, so an encoder restarting with a new SPS does not end the export.
Each keyframe carries them, so a keyframe whose sets never appeared ends it.
Other video waits for its first keyframe.

MPEG-TS export pads to the source's constant mux rate when the catalog
recorded one, or to `--mux-rate`, on a constant-rate schedule an IRD or groomer
can lock to. Each frame goes out as early as the receiver's buffers admit, up
to `--delay` ahead of its decode time and no further than its decoder buffer
holds. That send-ahead comes out of the delay, so the output trails a TS source
by `--delay`. A frame that cannot arrive in time at the rate fails the export,
as does video whose HRD declares a longer buffer delay than `--delay`; the
error names the `--delay` it needs.

## Play

```bash
moq --connect https://relay.example.com/anon --broadcast my-stream.hang play
moq ... play --delay 500ms          # fix the delay instead of measuring it
moq ... play --no-video             # audio only, without a window
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
the one playing.

The broadcast's announcement is its online signal, followed until the window
closes. Playback starts when the name is announced. A
[restarted publisher](#publisher-runs) starts it over on the new run, with a
fresh catalog, decoders, and clock. When the announcement ends, what is playing
finishes and the player waits for the name to return. The log names each run's
epoch. A catalog with nothing this build can play still ends the player.

Playback is behind the `play` feature, since it pulls in windowing and
audio-device dependencies:

```bash
cargo install moq-cli --no-default-features --features "quic,websocket,play"
```

`moq play` skips disabled renditions. If none are enabled, it waits for a later catalog. Disabling the current rendition stops it and selects an enabled replacement.

## Capture

```bash
moq --connect https://relay.example.com/anon --broadcast cam.hang import capture
moq ... import capture --display --system-audio          # share a screen with its sound (macOS)
moq ... import capture --window 39193 --no-audio         # one window (macOS, Windows, X11)
moq ... import capture --window --no-audio               # system window picker (Linux Portal)
moq ... import capture --camera 0 --width 1280 --height 720 --fps 30 --bitrate 3000000 --codec h265
```

Video goes through the platform hardware encoder with an H.264 software
fallback, and audio is Opus; [moq-video](/lib/rs/moq-video) lists the
backends. The camera opens only while someone is watching, and `--bitrate` is a
ceiling that backends with live bitrate control lower to fit the connection.
`moq devices` prints every source id. On Windows, `display:N` is an
enumeration index rather than a stable monitor id, so re-run `moq devices` if a
saved selector picks the wrong screen.

On Wayland, bare `--window` asks the system portal to choose one window;
there is no window id to look up or supply. An explicit `--window <id>` still
selects a native window, including XWayland windows through their X11 ids.
The portal may reuse its grant when capture reopens within the same process;
a new process shows the picker again. Bare `--window` requires a Linux build
with `pipewire`; other builds require an explicit window id.

Requires the `capture` feature. On Linux the microphone needs the ALSA
headers, and Portal screen/window capture and `pipewire:` cameras also need the `pipewire`
feature (links libpipewire).

## Transcode

```bash
moq --connect https://relay.example.com/anon --broadcast cam.hang transcode
moq ... transcode --rung 720:2500000 --rung 360:600000 --encoder nvenc --decoder nvdec
```

Rung sizes are the coded frame's short side. A 1080x1920 portrait source
produces 720x1280 and 360x640 rungs. Catalog rotation is preserved.

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
spans at group boundaries. An [untimed](/concept/moq-lite#subscriptions) track,
as every track is over moq-lite before 05 and moq-transport 14-16, is stored
one group per span and replays untimed. A store that already holds the recording is
continued, unless the source now announces another
[epoch](/concept/moq-lite#publisher-epochs): that is a restart, so the export
fails; record it under a new prefix. `--retention 1h` keeps only the last
hour, a DVR, which refuses an untimed track since it has no time to measure.

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

A restarted process takes the name at once: viewers see the broadcast restart,
and their next subscribe reaches the new run instead of waiting for its group
numbers to catch up. Epochs cross a connection only on moq-lite 07, which is
opt-in, and order by the host's clock there, so a host whose clock runs behind
the old run's waits for it to close. Older versions and moq-transport carry no
epoch, so a restart from another session is the newest route at an equal cost,
or a better one.

On moq-lite 07, two processes that pass the same `--epoch` (a UUIDv7, such as
`uuidgen -7` prints) are one publisher: relays fail over between them
mid-group, so they must produce identical tracks with aligned groups, which no
importer guarantees yet. Only `import fmp4`, `flv`, `hls`, and `ts` (one
program, or `--passthrough`) accept it. The RTMP, SRT, and WHIP ingests and
`import ts --program all` refuse it because they mint their own, and `import
avc3`, `capture`, `rtc --connect`, `archive`, and `transcode` because their
timestamps or bytes come from the instance rather than the input. Each that
accepts it announces once its catalog names a servable track, so a standby
started ahead of its feed is never asked for tracks it cannot serve yet. Input
that ends before then, such as an empty input or a PMT or header that
configures no track, exits 1 without announcing.

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
fetchable, which the [HLS gateway](/bin/hls) depends on. `export fmp4`, `export h264`,
and `export h265` take `--max-delay` (default 500 ms): how far a stalled group
may fall behind the live edge before *this* consumer skips it. Raising
`--max-age` never delays playback.
At stdin EOF, `import` keeps the finished broadcast up for its max-age, so a
subscriber still catching up can drain it; Ctrl-C ends that early.

`export ts`, `export flv`, and `export mkv` take `--delay` instead, like an SRT
receiver's latency, all tracks in decode order, so two exporters of one broadcast
emit the same order. FLV and MKV mux each frame that long after its decode time
(default 500 ms). TS output runs that far behind the source (default 1 s): the
send-ahead that fills a receiver's buffers comes out of the same budget, and 1 s
fits a broadcast-sized CPB. A source that sends no track ahead of its decode
time, as RTMP and browser publishers do, trails by the video's send-ahead on
top. A frame arriving after its deadline is dropped, and video resumes at its
next keyframe. The delay also bounds how stale a group may get, so `--delay 0`
writes each frame as soon as it is read and drops none as late, but still skips
a stalled group once a newer one is ready.
FLV and MKV reject their former `--max-delay` spelling with a migration
message. The output follows the source's clock within what ISO/IEC 13818-1
allows a TS PCR. A source further off fails the export, naming its rate, once
it has used half the delay; a brief network stall only drops what it made late.

`export ts --passthrough` reads the track an `import ts --passthrough` published
and writes its packets back out unchanged, from its first group on, each run
between two PCRs `--delay` after the first one's time on the source clock. The
output is exactly as conformant as the source, and two exporters emit the same
bytes. A run that arrives after its deadline is dropped and counted, leaving a
continuity error, and the rest carry on. It takes neither `--linger`,
`--stitch` nor `--mux-rate`. Only the hang catalog names the track, so an
`.msf` broadcast needs `--catalog-format hang`.

A stdout export ends with the broadcast. `export ts --linger 10s` waits that
long for the same publisher instance to come back, and carries on with the same
stream. A replacement (a restarted publisher, under a new
[epoch](/concept/moq-lite#publisher-epochs)) exits 1 unless `--stitch` follows
it as a full program switch: a new PMT from its catalog, with every PID flagging
the break. `--linger` also bounds a mid-stream stitch from the replacement
announcement through its first catalog snapshot; expiry exits 1. The default
`--linger 0s` follows only a replacement whose catalog is already available.
Without `--stitch` the export stays on the old publisher while it is
up. Only lite-07 sessions carry epochs, so on the default version every return
is a replacement. An export that fails while the broadcast stays up, such as on
a codec TS cannot carry, exits 1 without lingering. Only `ts` can mark a break,
so the other formats refuse `--linger`.

## Stats and feedback

`import --stats` names a stats track in the catalog and reports what each
rendition has written, about once a second while someone subscribes, with the
connection's RTT, estimated send rate, and loss. It covers the stdin imports;
capture, the gateways, `archive`, and `ts --program all` refuse it.

`export --echo <name>` answers a publisher that solicits feedback: under the
catalog's `echo` path it publishes `<name>.echo`, reporting what each rendition
the export reads has received and its newest arrival, until the export ends.
The name is one path segment. It covers the stdout container exports and reads
the solicitation from the hang catalog, so `--catalog-format msf` refuses it.

The `stats` and `echo` sinks print reports as JSON lines. `export stats`
follows one broadcast's stats track until the broadcast ends; `export echo`
prints `{"broadcast", "feedback"}` for every `.echo` broadcast under
`--broadcast` as they come and go.

```bash
moq ... --broadcast event import --stats ts < feed.ts
moq ... --broadcast event export stats
moq ... --broadcast event export --echo alice ts > out.ts
moq ... --broadcast viewers export echo
```

Counters are cumulative for the life of the process: a rendition that ends
keeps its final counts, and one that returns under the same ID continues them.
An fMP4 import counts samples as frames. Encoder counters (`skipped`,
`bitrate`) and playback feedback (`decoded`, `late`, `stalls`) are not reported
yet. [Stats](/concept/stats#media-reports) has the schema.

## Debugging

`RUST_LOG=debug` prints the negotiated version and every subscription.
`moq --connect <url> announced`, or `curl http://relay:4443/announced`, confirms the
relay is reachable and shows what it holds; see [Inspect a relay](/bin/inspect). Connection refused means UDP isn't getting through; certificate
errors on a dev relay want `--connect-tls-insecure` or the `http://`
fingerprint flow.
