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
| `import` | `hls <url>` | Follow a remote live HLS playlist. |
| `import` | `rtmp`, `srt`, `rtc` | Accept pushes (`--listen`) or pull from a remote (`--connect`). |
| `import` | `rtsp --connect <url>` | Pull an IP camera's stream. |
| `import` | `archive <url>` | Replay a recording from an object store. |
| `export` | `fmp4`, `mkv`, `ts`, `flv`, `h264`, `h265` | Write a container to stdout. |
| `export` | `hls --listen`, `hls --output <dir>` | Serve the broadcast as HLS over HTTP, record it to a directory, or both. |
| `export` | `rtmp`, `srt`, `rtc` | Serve plays (`--listen`) or push to a remote (`--connect`). |
| `export` | `archive <url>` | Record every broadcast under the prefix into an object store. |
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
`--connect <url>` dials a relay (the path is the auth path), `--connect-token`
presents a token (repeat it to present several, see
[Tokens](/bin/relay/auth#tokens)), and `--broadcast <name>` names the
broadcast. A `?jwt=` in the URL still works, and is how a token reaches a
relay that predates `--connect-token`. A process can
instead host sessions with `--listen`, or both at once, admitting clients the
way the relay does (see [Authentication](/bin/relay/auth)). Listening imports and
exports share the relay's shutdown behavior: SIGINT or SIGTERM sends accepted
sessions a reconnect-to-me GOAWAY and waits up to `--drain-timeout` (default
`10s`), exiting once they have all left. Sessions still open when the window
ends are closed, and the process exits once each has reported its `end` to the
auth server. Sessions on versions without GOAWAY, such as `moq-lite-03`, get
none and are closed the same way. A second
signal exits immediately; `--drain-timeout 0s` closes sessions immediately.
Pending handshakes and admission requests share the same drain window, and an
open stdin pipe does not keep the process alive afterward. Admission is part of
the handshake, so `--listen-timeout` bounds it too; `0s` leaves admission without
a deadline.

`moq import --help` lists the sources and `moq import rtmp --help` a specific
one; every flag is documented there. A `MOQ_*` environment variable `moq` does
not read, such as a misspelling or one meant for `moq-relay`, stops it at startup
with the variable named. Flags renamed between releases are listed in
[MIGRATE.md](https://github.com/moq-dev/moq/blob/main/rs/moq-cli/MIGRATE.md).

```bash
# Publish a file (remux to MPEG-TS without re-encoding)
ffmpeg -re -i video.mp4 -c copy -f mpegts -pes_payload_size 0 -muxdelay 0 - | \
    moq --connect https://relay.example.com/anon --broadcast my-stream.hang import ts

# Pull it back out
moq --connect https://relay.example.com/anon --broadcast my-stream.hang export ts | ffplay -

# With a token
moq --connect https://relay.example.com/rooms/1 --connect-token "$TOKEN" --broadcast alice.hang import ts
```

## Import

The `ts`, `fmp4`, and `flv` imports publish the input's own timestamps, mapped
to the wall time the first frame arrived. A timeline that rewinds, such as a
restarted encoder or a looping file wrapping to the top, ends the import with
an error; run it again to publish anew. A flagged MPEG-TS discontinuity that
jumps forward continues the broadcast and is declared on the exported clock.
MPEG-TS timestamps follow their source history across the 33-bit wrap
(every 26.5 hours), including files imported faster than real time and flagged
jumps, so a jump of more than half a wrap (13.25 hours) can read as a rewind.

For a source already in MPEG-TS, `ffmpeg -c copy -f mpegts` re-muxes and drops PIDs
(NIT, TDT/TOT, extra audio, teletext, SCTE-35), so pipe its bytes instead, for example
`tsp -I ip 239.1.2.3:1234 -O file - | moq --connect https://relay.example.com/anon --broadcast my-stream.hang import ts`,
or use [`import srt`](/bin/srt).

MPEG-TS import carries H.264/H.265 and AAC/MP2/AC-3/E-AC-3/Opus, and passes
SCTE-35, DSM-CC sections, and every other stream it does not decode (MPEG-2
video, LATM AAC, KLV, subtitles, DVB AC-3) through as tracks. MPEG-2 video still
paces the program by its timestamps, and a subscriber joins it at a sequence
header. H.264/H.265 AUs can span PES, and a PES can contain several. Supply
a PTS for each AU or declare a proportional POC clock: AVC needs
`fixed_frame_rate_flag` and `temporal_poc_flag` in the TS timing descriptor plus
fixed-rate SPS VUI; HEVC needs VUI's `vui_poc_proportional_to_timing_flag`. Missing timing
ends the import instead of repeating a timestamp. Private and undecoded PES go through whole, at their place on the program clock
(PCR); one before the first PCR is dropped. A stream it cannot carry unaltered
is refused with a warning: any in a program with no PCR, and one whose PES carry
an ESCR, pack header or TREF, are scrambled, leave a non-video PES unbounded,
or have a malformed header, as is `0x86` without a SCTE-35 registration, which
may be DTS audio. A PID no table claims, such as conditional access or ATSC
PSIP, is dropped with one warning and counted in the
[stats](/concept/stats#mpeg-ts-sections); `--passthrough` keeps them.
SCTE-35 cues and private PES share the media clock, or anchor on the first PCR
when the PMT declares only data. Audio-only and data-only programs round-trip
without video; a data-only program still requires PCR. A program declaring
media waits for its timestamp and is refused if input ends or declares a clock
break before one arrives.
PES and untimed section staging share a bounded budget, so a source that never
supplies a PES boundary or clock fails rather than retaining bytes indefinitely.
It takes one program: a multi-program
stream is refused until `--program` picks one, or `--program all` publishes
each as its own broadcast (`event.hang` becomes `event/1.hang`,
`event/2.hang`), following the PAT: a program it adds publishes, one it drops
ends, and one that returns publishes as a new instance. A listed program whose
PMT does not arrive within 0.5 s of program clock ends the import. A damaged packet is
dropped on its own PID, so video freezes until its next keyframe while the
other tracks carry on. The importer also
logs feed health (stalled streams, TR 101 290 errors) without changing what it
publishes. `--passthrough` publishes the multiplex whole instead, every packet
in order on one [`m2ts`](/concept/hang#transport-streams) track with every
program aboard, for what demultiplexing cannot carry, such as a scrambled
service; `export ts --passthrough` writes it back out. FLV accepts the same
legacy and enhanced-RTMP codecs as the [RTMP gateway](/bin/rtmp).

fMP4 import splits each fragment into one frame per sample, so players need no
MP4 parser. `import fmp4 --cmaf` publishes each fragment whole as
[`cmaf`](/concept/hang#container) instead, keeping the source's exact sample
timing.

`import rtsp` pulls an IP camera's H.264 or H.265 video and AAC audio without
re-encoding. RTP rides the RTSP connection, so run it on the camera's network
and the camera needs no inbound port. Credentials go in the URL
(`rtsp://user:pass@camera/stream`), which other local users can read from the
process list. Audio nothing in the stack plays, such as G.711, is skipped with
a warning, and the broadcast goes on video-only. It reconnects with backoff,
including when the camera sends no media for 10 seconds or no keyframe for 30,
until a fatal error (refused access, or no stream it can publish), and
each RTSP session is a new broadcast with its own catalog, since the session's
RTP time restarts; a camera whose clock jumps mid-session is restarted the same
way. Embed [`moq-rtsp`](https://github.com/moq-dev/moq/tree/main/rs/moq-rtsp) for your own supervision,
such as two of a camera's URLs as renditions of one broadcast.

```bash
moq --connect https://relay.example.com/anon --broadcast camera.hang \
    import rtsp --connect rtsp://admin:secret@192.168.1.64/stream1
```

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
moq ... play --no-video --metrics play.ndjson   # log what the speaker played
```

Decodes H.264, H.265, and AV1 video with the platform hardware decoder where
available, and Opus, PCM, and AAC-LC audio in software. The opt-in `openh264`
feature adds software H.264, which a host without a hardware decoder needs, and
the opt-in `vpx` feature adds software VP8 and VP9 through the build host's
libvpx. The log
names the decoder each track opened.

Playback starts at the newest group and trails the live edge by `--delay`. The
default, `auto`, sizes the buffer to how unevenly audio arrives, with the same
[algorithm](/concept/audio-jitter) as the browser player. A duration fixes the
delay and is also how long a stalled group is waited on before it is skipped.
Each role follows the catalog, switching rendition when the publisher retires
the one playing. With `--no-video`, `--metrics <PATH>` writes a JSON line every
250 ms with the playhead, the delay, and every gap and stall the speaker hit, in
the schema the
[audio quality harness](https://github.com/moq-dev/moq/tree/main/test/audio-quality)
reduces to underruns, skips, and silence.

The broadcast's announcement is its online signal, followed until the window
closes. Playback starts when the name is announced. A
[restarted publisher](#publisher-runs) starts it over on the new run, with a
fresh catalog, decoders, and clock. When the announcement ends, what is playing
finishes and the player waits for the name to return. The log names each run's
epoch. A refused broadcast request or a catalog with nothing this build can
play ends the player.

Playback is behind the `play` feature, since it pulls in windowing and
audio-device dependencies:

```bash
cargo install moq-cli --no-default-features --features "quic,websocket,play,openh264"
```

`moq play` skips disabled renditions when it picks one. If no enabled rendition
matches its name and codec filters, it waits for a later catalog. Disabling the
current rendition switches to an enabled one of its kind that matches the
filters; with none, it stays subscribed, so re-enabling it (an unmute) plays at
once.

## Capture

```bash
moq --connect https://relay.example.com/anon --broadcast cam.hang import capture
moq ... import capture --display --system-audio          # share a screen with its sound (macOS)
moq ... import capture --window 39193 --no-audio         # one window (macOS, Windows, X11)
moq ... import capture --window --no-audio               # system window picker (Linux Portal)
moq ... import capture --camera 0 --width 1280 --height 720 --fps 30 --bitrate 3000000 --codec h265
moq ... import capture --on-demand                       # capture only while someone watches
```

Video goes through the platform hardware encoder, with an H.264 software
fallback when built with `openh264`, and audio is Opus; [moq-video](/lib/rs/moq-video) lists the
backends. The camera and microphone stay open and encode while publishing,
so a late subscriber can fetch the recent past. `--on-demand` captures and
encodes each source only while its track has a subscriber, and leaves nothing
to fetch from before the first viewer; video still opens its source once at
startup to probe the mode it negotiates. `--bitrate` is a ceiling that
backends with live bitrate control lower to fit the connection.
`moq devices` prints every source id. On Windows, `display:N` is an
enumeration index rather than a stable monitor id, so re-run `moq devices` if a
saved selector picks the wrong screen.

On Wayland, bare `--window` and bare `--display` ask the system portal to
choose one window or screen; there is no id to look up or supply. An explicit
`--window <id>` or `--display <id>` still selects a native source, including
XWayland's through their `x11:` ids, and any other id fails rather than opening
the picker. The portal may reuse its grant when capture reopens within the same
process; a new process shows the picker again. Bare `--window` requires a Linux
build with `pipewire`; other builds require an explicit window id.

Requires the `capture` feature, plus `openh264` on a host without a hardware
encoder. On Linux the microphone needs the ALSA
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
the `transcode` feature, plus `openh264` on a host without a hardware encoder.
A restarted source retires the old ladder and publishes a fresh derivative
instance at the same output path. A clean end waits for the source to return;
stop the command to finish following. A refused source request or an encoding
failure ends the command.

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
# Record every broadcast announced under `event`
moq --connect https://relay.example.com/anon --broadcast event export archive s3://recordings

# Replay one recorded broadcast under another name
moq --connect https://relay.example.com/anon --broadcast event-replay.hang import archive s3://recordings/event/@<epoch>
```

`export archive` records every broadcast announced at or under `--broadcast`
with [moq-archive](https://docs.rs/moq-archive), each publisher instance under
`<broadcast>/@<epoch>` in the store, until the connection ends. Like
`announced`, it leaves out a broadcast whose name starts with `.` unless
`--broadcast` names it. A broadcast path segment that starts with `@` is
stored as `%40`, so no recording lands inside another. Every track is stored as its own objects, cut about every 2 s
of media (or wall clock for an [untimed](/concept/moq-lite#subscriptions)
track) and at least every 4 s, splitting a long group between frames. A broadcast restarted under a new
[epoch](/concept/moq-lite#publisher-epochs) records under its own key; a route
without an epoch gets a fresh one each time it announces, so it never resumes
or stitches into an older recording. Any recording that fails ends the export.
`--retention 1h` keeps only the last hour, a DVR, deleting older objects
`--retention-grace` (default 30 s) after they leave the window.

`import archive` republishes one recorded broadcast, the `<broadcast>/@<epoch>`
prefix `export archive` wrote, as an ordinary broadcast under a new epoch: every
track, the catalog included, with its original groups, frames, timescale, and
priority. Each track is live from its newest stored group and serves older
groups from the store on request, with no max age, so the whole recording stays
reachable. A recording still being made is followed, checked every `--refresh`
(default 1 s), until `export archive` marks it complete; one whose export
stopped early stays live. It refuses a prefix with no recording, or with keys
`export archive` does not write.

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
    -- export --broadcast event.hang archive file:///recordings
```

## Publisher runs

Each run of `moq` announces a fresh
[publisher epoch](/concept/moq-lite#publisher-epochs), kept across reconnects.
The RTMP, SRT, and WHIP ingests mint one per connection instead, `import
rtsp` one per RTSP session, and `import ts --program all` one per program.

A restarted process takes the name at once: viewers see the broadcast restart,
and their next subscribe reaches the new run instead of waiting for its group
numbers to catch up. Epochs cross a connection only on moq-lite 07, which is
opt-in, and on moq-transport, and order by the host's clock
there, so a host whose clock runs behind the old run's waits for it to close.
Other versions carry no epoch, so a restart from another session is the newest
route at an equal cost, or a better one.

Each independent importer publishes its own epoch, even when two importers read
one stream and publish one path. See [Redundant ingest](/setup/redundancy) for
same-path switching, subscriber-controlled selection on separate paths, and
several routes to one publisher.

An importer announces once its catalog names a servable track. Input that ends
before then, such as an empty input or a header that configures no track,
exits 1 without announcing.

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
so it needs no `--connect`. WAN cluster dials (`--cluster-connect`,
`--cluster-api-url`) share `--connect-bind` with `--connect`, and each binds
it, while LAN dials take an ephemeral port. So a fixed `--connect-bind` port with
both `--connect` and a WAN cluster peer is refused at startup: use port 0, or drop
`--connect` (unset `MOQ_CONNECT` if it supplies the URL).

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
At stdin EOF, `import` keeps the finished broadcast announced for
`--linger` (default 30 s) from the end, so a subscriber that arrives late, such
as a player that read the final catalog after the end, still gets the tail;
`--linger 0s` closes it at once, and Ctrl-C ends the wait early. A gateway
import does the same at a clean end: an RTMP unpublish (`deleteStream`) or
completed play, the end of an SRT stream, a WHIP `DELETE`, or an HLS playlist
ending with `EXT-X-ENDLIST`. A dropped connection or an error closes the
broadcast at once. A reconnect publishes a newer
[epoch](/concept/moq-lite#publisher-epochs) at the same path, which takes over
the path while the old broadcast finishes its linger. `import rtsp` redials
after every session, so it closes each run at once and refuses `--linger`, as
do `import archive`, `import capture`, and a WHEP pull (`import rtc --connect`).

`export ts`, `export flv`, `export rtmp`, and `export mkv` take `--delay` instead, like an SRT
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
FLV, MKV and RTMP refuse the released `--max-age` spelling with a migration
message. The output follows the source's clock within what ISO/IEC 13818-1
allows a TS PCR. A source further off fails the export, naming its rate, once
it has used half the delay; a brief network stall only drops what it made late.
A timeline that steps further than the delay absorbs with no restart marker, as
a changeover between two encoders whose clocks disagree, re-acquires the clock
as at startup rather than read the step as drift.

`export ts --passthrough` reads the track an `import ts --passthrough` published
and writes its packets back out unchanged, from its first group on, each run
between two PCRs `--delay` after the first one's time on the source clock. The
output is exactly as conformant as the source, and two exporters emit the same
bytes. A run that arrives after its deadline is dropped and counted, leaving a
continuity error, and the rest carry on. It takes neither `--follow`,
`--stitch` nor `--mux-rate`. Only the hang catalog names the track, so an
`.msf` broadcast needs `--catalog-format hang`.

A stdout export ends with the broadcast. `export ts --follow 10s` waits that
long for the same publisher instance to come back, and carries on with the same
stream. A return that refuses its catalog keeps it waiting for the next
announcement; if the follow runs out first, it exits 1 with that refusal. One
that goes before serving, such as one still routed through a dead session, is
no return. A replacement (a restarted publisher, under a new
[epoch](/concept/moq-lite#publisher-epochs)) exits 1 unless `--stitch` follows
it as a full program switch: a new PMT from its catalog, with every PID flagging
the break. A stitch waits for the replacement's first catalog snapshot until it
arrives or the replacement goes, keeping the old publisher playing meanwhile,
so even the default `--follow 0s` switches to a replacement that is still
starting. After the catalog arrives, the old instance keeps feeding for up to
`--follow` while the replacement acquires its clock and first usable media.
The marked switch then drops the old instance's buffered tail, so the
replacement plays at `--delay` rather than behind it. A replacement usable
within that window adds no gap to the output; one still acquiring when it runs
out, such as on a link too slow to carry both instances, starts after a gap.
Timestamps across epochs are not aligned. Without `--stitch` the export stays
on the old publisher while it is up. Only lite-07 and moq-transport sessions
carry epochs, so on the
default version every return is a replacement. A failure other than the
publisher leaving, such as a codec TS cannot carry, exits 1 at once. Only `ts`
can mark a break, so the other formats refuse `--linger`, the released spelling
of `--follow`.

## Reports and feedback

`import --report` names a track in the catalog's `stats` section and reports
what each rendition has written, about once a second while someone subscribes,
with the connection's RTT, estimated send rate, and loss. It covers the stdin imports;
capture, the gateways, `archive`, and `ts --program all` refuse it.

`import --echo-path <path>` solicits feedback from the same imports: it sets
the catalog's `echo` path, resolved against the broadcast like a relative URL,
so `viewers` from `room/event` is `room/viewers`. A path above the connection
root is refused, since readers would refuse the catalog, and so is one no
viewer could publish beneath: a segment holding `*`, or past 32 segments.
`export --echo <name>` answers: under the catalog's `echo` path it publishes
`<name>.echo`, reporting what each rendition the export reads has received and
its newest arrival, until the export ends. It covers the stdout container
exports and needs a hang catalog, so MSF refuses it.

The `report` and `echo` sinks print reports as JSON lines. `export report`
follows one broadcast's report track until the broadcast ends; `export echo`
prints `{"broadcast", "feedback"}` for every `.echo` broadcast under
`--broadcast` as they come and go, reading one that ends for 5 s more for its
final report. Either sink stops reading a report that grows past 1 MiB.

```bash
moq ... --broadcast event import --report --echo-path viewers ts < feed.ts
moq ... --broadcast event export report
moq ... --broadcast event export --echo alice ts > out.ts
moq ... --broadcast viewers export echo
```

Counters are cumulative for the life of the process: a rendition that ends
keeps its final counts, and one that returns under the same ID continues them.
A report holds 1,024 rendition IDs at most: past that, each new ID replaces
one that has ended.
[Stats](/concept/stats#media-reports) has the schema.

A TS import adds an `mpegts` section: each elementary stream's access units
and quiet time, and the TR 101 290 counters it logs. A TS export adds its
streams' rows and a `release` section: frames dropped late, the margin frames
arrive with, and its clock's offset and drift from the source. A passthrough
reports `release` alone at export and neither at import. See
[MPEG-TS sections](/concept/stats#mpeg-ts-sections).

## Debugging

`RUST_LOG=debug` prints the negotiated version and every subscription.
`moq --connect <url> announced`, or `curl http://relay:4443/announced`, confirms the
relay is reachable and shows what it holds; see [Inspect a relay](/bin/inspect). Connection refused means UDP isn't getting through; certificate
errors on a dev relay want `--connect-tls-insecure` or the `http://`
fingerprint flow.
