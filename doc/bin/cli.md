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
instead host sessions with `--listen`, or both at once. A listener admits
clients by `--auth-url` or `--auth-public`, as the relay does (see
[Authentication](/bin/relay/auth)); public rules ignore certificates, so
`--auth-public` refuses to start with `--listen-tls-root`, and only `--listen`
(QUIC) verifies one. `moq import --help` lists the sources and `moq import rtmp --help` a specific one.

```bash
# Publish a file (remux to MPEG-TS without re-encoding)
ffmpeg -re -i video.mp4 -c copy -f mpegts -pes_payload_size 0 -muxdelay 0 - | \
    moq --connect https://relay.example.com/anon --broadcast my-stream.hang import ts

# Pull it back out
moq --connect https://relay.example.com/anon --broadcast my-stream.hang export ts | ffplay -

# With a token
moq --connect "https://relay.example.com/rooms/1?jwt=$TOKEN" --broadcast alice.hang import ts
```

The `ts`, `fmp4`, and `flv` imports publish the input's own timestamps, and
the catalog clock maps the first frame to the time it arrived. So a feed whose
PTS starts hours in still names the right wall time, and every track keeps its
offset from the others. A group starting before the previous group's start,
such as a restarted encoder or a looping file wrapping to the top, ends the
import with an error; run it again to publish anew. A keyframe that merely
overlaps the previous group's last frame is not a rewind.

MPEG-TS import carries H.264/H.265 and AAC/MP2/AC-3/E-AC-3, passes SCTE-35 and
subtitle PIDs through as tracks, and round-trips the service tables. A
`discontinuity_indicator` on the program's PCR PID is a system time-base reset,
so it breaks every track's timeline and the exported clock declares the break in
turn. A new timeline whose first group starts before the previous group's start
ends the import like any other rewind. The same flag on an elementary PID other than the program PCR PID, a
continuity-counter gap, and the 33-bit timestamp rollover move no clock and
declare nothing. FLV covers H.264 + AAC.

`import ts` samples each elementary stream's access-unit count once a second and
logs a stream whose count stopped advancing, with how long it has been quiet on
the program clock, once per silence. The mux can keep flowing, PCR and
continuity intact, around a PID that delivers nothing, and no transport check
downstream sees it. A sparse stream such as SCTE-35 goes quiet between cues, so
the line reports rather than alarms; `Import::stats` carries the same counters
for a caller that sets its own limit.

`import ts` also counts the ETSI TR 101 290 errors of the feed it receives, at
that standard's fixed limits: `TS_sync_loss`, `Sync_byte_error`, `PAT_error`,
`Continuity_count_error`, `PMT_error`, `Transport_error`, `CRC_error` (PAT and
PMT), `PCR_repetition_error`, `PCR_discontinuity_indicator_error` and
`PTS_error`. They are cumulative, logged with their totals in the sample after
any of them moves, and carried on `Import::stats` stream-wide and per PID. They
change nothing that is published. They grade the stream as it reached the
importer, not the wire a receiver sees downstream, and the PCR checks grade
consecutive PCR values rather than arrival times, so they speak for the encoder
and not for the network in front of the importer. Table, PCR and PTS intervals
run on the program clock, which starts at the first PCR after the PMT.
`PID_error` is covered, more strictly, by the access-unit counts above.
`PCR_accuracy_error` is not measured.

A corrupt media packet, malformed PES header, or damaged codec access unit is
refused whole and counted in the PID's cumulative `damaged` counter, beside
`resyncs`, `discarded`, and `unconfirmed`. Ingest continues on every other PID.
Video closes its group at the break and resumes at its next keyframe, as it does
after a continuity-counter gap: the pictures in between may reference the lost
one, so they are dropped rather than decoded with artefacts. Each break freezes
video for up to one GOP. The shared TS stats log reports each counter increase,
including at the end of input. Publishing, catalog, and clock errors still end
the import.

MPEG-TS import takes one program. A multi-program stream is refused before
anything is published, naming its programs, rather than merged onto one clock;
a PAT that adds a program mid-stream ends the import the same way.
`--program 2` imports program 2 alone. `--program all` publishes each program
the first PAT lists as its own broadcast, with its own clock and catalog, keeping
the catalog suffix last: `--broadcast event.hang` publishes `event/1.hang`,
`event/2.hang`, and so on. `export ts` writes one program per broadcast.
`import srt` takes the same `--program`.
A selected program's SI describes that service alone: its SDT lists only the
selected service, and other services' EIT is dropped. Network-wide tables (NIT,
BAT, TDT/TOT, and the SDT and EIT of other transport streams) pass through.
SI matches the selection by DVB `service_id`, which is assumed to equal the PAT
`program_number`.

```bash
moq --connect https://relay.example.com/anon --broadcast event.hang import ts --program all < mux.ts
```

MPEG-TS export restarts its clock and table cadence after a declared marker,
discarding the old mux buffer. The first new clock packet signals the break and
stdout pacing re-anchors. Every rendition joins the new program generation;
no track is fenced across the marker.

MPEG-TS export frames AAC as ADTS, which labels only the AAC Main, LC, SSR,
and LTP profiles. HE-AAC and HE-AACv2 go out as their AAC-LC core, and decoders
find the SBR and PS in band, as ffmpeg's ADTS output does. A track whose
profile or channel layout ADTS cannot label is refused rather than mislabeled.
An Opus track is labeled with the plain channel code its extension descriptor
can name: a family 0 head, a family 1 head with the Vorbis mapping, or mono or
stereo when the track has no OpusHead. Any other head is refused rather than
written with a guessed channel code.

A constant-rate MPEG-TS source records its multiplex rate in the catalog
(`mpegts.muxRate`, measured off the PCR clock, null stuffing included), and
`export ts` pads its output with null packets back to that rate so an IRD or
groomer receives a constant-rate stream. `--mux-rate 5000000` pads to an explicit
rate instead, including for a broadcast that recorded none. Media is never delayed
or dropped to fit: a source that sustains more than the rate overruns it, and a
VBR source records nothing, so export without either stays unpadded.

The `fmp4`, `mkv`, `flv`, `h264`, and `h265` exports select renditions with
flags before the sink: `--video-name` and `--audio-name` pick a rendition,
`--video-codec` and `--audio-codec` keep a codec family, and `--no-video` or
`--no-audio` leaves a role out. `h264` and `h265` refuse `--no-video` and the
audio selection flags. `ts`, `archive`, and the gateways don't apply selection, so they
refuse these flags.

```bash
moq ... export --no-video fmp4 > audio.mp4
moq ... export --video-name hd --no-audio mkv > hd.mkv
```

fMP4 export writes one fragment per publisher group on each track. Audio follows
the publisher's cuts; video normally follows GOPs. Closing a group flushes it
even when the live publisher pauses. `--fragment-duration 2s` caps
the fragment span as frames arrive, including audio whose publisher never cuts.
MKV uses the same flag to cap clusters, which otherwise follow video GOPs.

The fMP4 init segment declares every rendition in the catalog, so it waits until
each can be described. An Annex-B H.264 or H.265 track, or video whose catalog
leaves out its dimensions, waits for its first keyframe; the other tracks keep
reading meanwhile and their fragments follow the init. A track that is still
waiting once another has queued 30 seconds fails the export. After the init the
track set is fixed: a rendition that leaves and returns with the same codec
configuration is written under its original track, while a new rendition, a
changed configuration, or a return that replays media already written ends the
export with an error naming it. Restart the export to pick up a new rendition.
An Opus rendition declared without its OpusHead gets a guessed pre-skip. A
head that arrives later with the same channel count, decode rate, and gain is
accepted even if its pre-skip and input rate differ. An init already written
keeps the guess, and later heads must match the first one.

## Play

```bash
moq --connect https://relay.example.com/anon --broadcast my-stream.hang play
moq ... play --delay 500ms          # fix the delay instead of measuring it
moq ... play --no-video             # audio only
```

Decodes H.264, H.265, and AV1 video using the platform hardware decoder where
available, and Opus, PCM, and AAC-LC (mono or stereo) audio in software. The
opt-in `vpx` feature adds software VP8 and VP9 (8-bit 4:2:0) through libvpx,
which the build host must provide. The log names the decoder each track
opened. `--video-name` and `--audio-name` pick a rendition, and `--no-video` or
`--no-audio` leaves a role out. `--no-video` still opens a window, which stays
blank; closing it stops playback. HE-AAC signaled only in band (implicit SBR, as
over MPEG-TS) plays as its half-rate AAC-LC core.

Playback runs on a clock it owns. `--delay` is how far it trails the live
edge: the jitter a late frame may absorb. The default, `auto`, measures how
unevenly audio arrives and sizes the speaker's buffer to match, using the same
[algorithm](/concept/audio-jitter) as the browser player, so a publisher that
flushes 100 ms at a time gets a buffer deep enough to play through the next
flush. It waits up to 2 s on a stalled group before skipping it, since a budget
any shorter would hide the very lateness it measures; a broadcast with no audio
has nothing to measure, so video trails by 100 ms and skips past that. A duration fixes the delay
instead, and doubles as the point past which a stalled group is skipped. The
speaker holds the delay, with a 50 ms floor under it: it pads back up to the
delay after running dry and skips back down onto it after a burst. The picture
is scheduled against where the speaker actually is. While video owns the clock, a frame arriving earlier than predicted
pulls playback forward, so a late start catches up to live instead of staying
behind it. Once the speaker owns the clock, video follows the speaker instead.

Video receives encoded frames independently of decoding, so a tune-in burst can
update that clock even while the window is waiting for its first picture.
Encoded video is retained within the delay budget with byte accounting; a skip
resumes at a keyframe. Decoding starts 100 ms before the earliest picture still
owed is due, so B-frame reordering and pictures the decoder holds back are
covered however deep they go. The window holds at most three decoded pictures;
a larger decoder batch waits for room rather than pushing out pictures not yet
shown. A stalled window loses its oldest picture once a newer one is due,
instead of blocking reception. The configured delay therefore does not turn
into seconds of raw video surfaces.

Each role follows the catalog for as long as it lasts. Each decoder starts at
the newest cached group, including when a rendition is reopened, so playback
does not replay the retained backlog. A publisher that retires the rendition
being played ends that track and the role picks a replacement. A retired audio
rendition plays out what the speaker holds while its replacement fills, so the
switch does not cost a delay of silence. Playback is
behind the `play` feature, since it pulls in windowing and audio-device
dependencies:

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

Video goes through the platform hardware encoder (VideoToolbox, Media
Foundation, NVENC, and with the opt-in `vaapi` / `v4l2` features VAAPI and V4L2
M2M) with a built-in H.264 software fallback;
audio is Opus. The camera is opened only while someone is watching, and
`--bitrate` is the opening ceiling. Backends with live bitrate control lower it
to fit the connection's bandwidth estimate. `moq devices` prints every source
id. Requires the `capture` feature; on Linux that needs the ALSA headers for
the microphone, and `--display` and `pipewire:` cameras also need the
`pipewire` feature (links libpipewire).

On Windows, display and window capture use Windows.Graphics.Capture and
require Windows 10 2004 (build 19041) or newer. Cursor capture follows the
capture configuration. The system capture border stays visible unless the OS
supports borderless capture and grants access. Frames are converted to NV12
on the GPU; software encoding reads them back. Windows application capture
and system audio are separate capabilities, not enabled by this backend.
Windows `display:N` selectors are enumeration indices; switching from Desktop
Duplication to WGC can change which monitor a saved selector names. Run
`moq devices` again and reselect the intended display after upgrading.

## Transcode

```bash
moq --connect https://relay.example.com/anon --broadcast cam.hang transcode
moq ... transcode --rung 720:2500000 --rung 360:600000 --encoder nvenc --decoder nvdec
```

Publishes `cam.hang/transcode.hang` whose catalog references the source's
rendition and adds lower rungs that are decoded and encoded only while someone
watches them. On NVIDIA the whole pipeline stays on the GPU; `--frames cpu`
forces decoded frames into CPU memory instead of the default `native`.
Requires the `transcode` feature.

The source is the largest rendition this host can decode with `--decoder`, so a
software-only host transcodes from an H.264 rendition rather than a larger H.265
or AV1 one. When no rendition decodes, the command exits naming the decoder's
refusal.

The ladder is sized against the source picture and follows it, so a source that
changes resolution mid-stream (a window capture renegotiated by a resize, a
publisher reconnecting at a new size) resolves the rungs again. Rungs that still
fit keep serving. A rung the new picture has no room for finishes its track, as
does one whose own picture moved, and the latter comes back under a new name
(`video/360p.2`), so a viewer on either reselects as it would on any other
rendition change.

Custom `--rung` values may be supplied in any order. Heights round down to even;
heights and bitrates must then increase strictly together. Duplicate heights or
bitrates, inverted rankings, and zero-sized or zero-bitrate rungs are rejected
before connecting.

## Announced

```bash
moq --connect https://relay.example.com/anon announced
moq ... announced room --json
```

Follows the broadcasts announced on a relay over MoQ, with the session's own
auth: the live counterpart of the relay's HTTP `/announced/<prefix>`. Paths are
relative to the `--connect` path. On a terminal it shows what is announced
under `prefix` right now, redrawn as broadcasts start and end, and a list
taller than the terminal ends in a count of the rest. Piped, it prints
`+ path` for each broadcast already announced, then `+ path` and `- path` as
broadcasts come and go. `--json` prints `{"path": "room/alice", "active": true}`
per line instead, on a terminal or not. It runs until interrupted, and exits
non-zero if the session ends.

Like `/announced`, it follows announced prefixes, which by convention are
broadcast paths. A new route to a path already announced prints nothing. A name
starting with `.` stays hidden unless `prefix` names it. `announced` only dials
`--connect`, and refuses any other MoQ-side flag.

[Inspect a relay](/bin/inspect) walks through `announced` and `fetch` next to
their `curl` equivalents, including reading the relay's stats.

## Fetch

```bash
moq --connect https://relay.example.com/anon --broadcast my-stream.hang fetch catalog.json | jq
moq ... fetch video/hd --group 42 --json
```

Writes one group of a track to stdout over MoQ, with the session's own auth:
the counterpart of the relay's HTTP `/fetch/<broadcast>/<track>?group=N`. Without
`--group` it reads the newest group. By default stdout carries the frame
payloads back to back, byte for byte what `curl` gets from `/fetch`. `--json`
prints one line per frame instead:
`{"group": 42, "frame": 0, "size": 1234, "payload": "<base64>"}`, with a
zero-based `frame` and padded standard base64.

`<track>` is the literal track name. `/fetch` splits its path on the last `/`,
so the two agree only for names without one. Fetch only dials `--connect`, and
refuses any listener, cluster, auth, or `--hop` flag. It gives up after 30
seconds, as `/fetch` does, and exits non-zero when the broadcast or group is not
found (before writing anything), the relay refuses, or the deadline passes.

## Archive

```bash
# Record a broadcast until it ends
moq --connect https://relay.example.com/anon --broadcast event.hang export archive s3://recordings/event

# Replay it under another name
moq --connect https://relay.example.com/anon --broadcast event-replay.hang import archive s3://recordings/event
```

`export archive` records one broadcast with
[moq-archive](https://docs.rs/moq-archive), reading its catalog as it changes.
Every track gets its own timeline, stored in spans cut at group boundaries
between 2s and 10s. The catalog and every text, JSON, and binary track are
sparse data, so each of their groups is stored as soon as it finishes, and a
group that never closes is stored in pieces as it grows. It refuses a rendition served
from another broadcast, and one that returns after the catalog dropped it. The
stage ends once the broadcast does. A store URL that already holds a
recording is continued: each track resumes after its newest stored span. `--retention 1h` keeps only the last hour (a DVR),
deleting expired objects, and timeline objects no longer needed to recover it,
`--retention-grace` (default 30s) after the timeline stops needing them. Every
track keeps at least its newest span, so a catalog that never changes outlives
the video it was published with.

`import archive` republishes a recording: each track's timeline replays as a
live track and every track's groups are served on request, one object GET per
stored span. By default it replays what is stored and ends the timelines there;
`--follow 2s` keeps checking for new spans, and newly recorded tracks, of a
recording still being made.

Store URLs are `file:///absolute/path`, `s3://bucket/prefix`,
`gs://bucket/prefix`, or `az://container/prefix`. Cloud credentials come from
the usual `AWS_*`, `GOOGLE_*`, and `AZURE_*` environment variables. The `s3`,
`gcs`, and `azure` cargo features are on by default.

## Multiple stages

Separate stages with `--` to bridge several broadcasts, or both directions,
over one connection:

```bash
moq --connect https://relay.example.com/anon \
    import --broadcast event.hang srt --listen 0.0.0.0:9000 \
    -- export --broadcast event.hang hls --listen 0.0.0.0:8080 \
    -- export --broadcast event.hang archive file:///recordings/event
```

## Redundant publishers

Two publishers of the same broadcast name are interchangeable sources:
relays hold both routes and fail over between them mid-group. They must
produce identical tracks with aligned groups. A restarted encoder is the same
broadcast too, so one whose groups restart from 0 must publish under a new
name, or viewers wait for its sequence to catch up.

## Cluster

The CLI reads the same `--cluster-*` flags as `moq-relay`, LAN and WAN alike,
and publishes on the cluster origin. A `moq --cluster-lan` process and a
`moq-relay` with `[cluster.lan]` on the same network mesh with each other.

`--cluster-lan` advertises this process on the LAN over mDNS and meshes with
every other participating MoQ process. It reuses `--listen`, filling in an
ephemeral port and a generated certificate when those are unset. A LAN peer
authenticates with its mDNS credential; `cluster.token` is for
`--cluster-connect` and `--cluster-connect-api` peers only.

```bash
moq --cluster-lan import capture
moq --cluster-lan --cluster-lan-secret /etc/moq/cluster.key import capture
```

`--cluster-lan-secret` restricts the mesh to peers holding the same key.
Without it, anyone who can reach the listener joins, so leave it unset only
on networks you trust. mDNS is still an open channel: the secret
authenticates the record, it does not hide the credential or the node URL.

`--cluster-lan-app` names the DNS-SD application this process advertises
under. Peers using a different name never discover this one. It defaults to
`default`, which moq-relay shares, so the two find each other with no
configuration. An application built on the library picks its own name.

The WAN flags (`--cluster-connect`, `--cluster-connect-api`, `--cluster-node`,
`--cluster-token`, `--cluster-id`, `--cluster-tier`) match the relay. `--cluster-connect` and `--cluster-connect-api` are a MoQ side on
their own, so `moq --cluster-connect https://relay.example import ts` needs
no `--connect`. See [Clustering](/bin/relay/cluster).

## Auth

```bash
moq auth generate --algorithm ES256 --out private.jwk --public public.jwk
moq auth sign --key private.jwk --root rooms/123 --publish 'alice/**' --subscribe '**' > alice.jwt
moq auth verify --key public.jwk --in alice.jwt
```

`--publish` and `--subscribe` take patterns: `alice` is one broadcast,
`alice/**` is a subtree, `**` is everything under `--root`.

`moq auth serve` answers a relay's auth requests with the same keys, public
rules, an explicit mTLS grant, tiers, and session limits; see
[Auth server](/bin/relay/auth#auth-server).

```bash
moq auth serve --listen 127.0.0.1:4440 --key-dir keys/ --public-subscribe 'anon/**'
```

`moq auth serve` also accepts `--key-set keys.jwks` to verify tokens from a JWK Set.

`moq auth sessions` and `moq auth revalidate` talk to a relay's internal
listener. A push is a re-check: the auth server's reply is what kicks. An
empty filter is every session on that node.

```bash
# Kick one session by id.
moq auth revalidate --internal-url http://127.0.0.1:9101 --id 00ff

# Re-check everyone under a path.
moq auth revalidate --internal-url http://127.0.0.1:9101 --path 'rooms/123/**'
moq auth sessions --internal-url http://127.0.0.1:9101 --path 'rooms/123/**'
```

See [Authentication](/bin/relay/auth).

## Retention and latency

`import --max-age` (default 30 s) tells relays how long to keep old
groups fetchable, which the [HLS gateway](/bin/hls) depends on. `export --max-age` (default 500 ms) is how long *this* consumer waits for a
stalled group before skipping. Raising the first never delays playback.

For `export ts`, `--max-age` also bounds how long the muxer holds a leading
track for a lagging one. Frames go out in media-time order across all tracks,
not arrival order, so two exporters of one broadcast emit them in one order. A
track quiet for longer is muxed around until it catches up; a sparse track
(SCTE-35) costs that wait once per cue. `--max-age 0` keeps arrival order.

A stdout export ends with the broadcast. `export ts --linger 10s` waits that
long for the broadcast to come back instead: a publisher that restarts within
it is picked up under the same PIDs, with the break flagged (PCR discontinuity,
PAT/PMT re-sent). Nothing is written while it is gone. When the linger runs out,
the exit code is that of the last end: 0 if the broadcast finished cleanly, 1 if
it dropped or failed. The default is `0s`, which exits on the first end the same
way. Only `ts` can mark the restart, so the other formats refuse `--linger`.

## Debugging

`RUST_LOG=debug` prints the negotiated version and every subscription.
`moq --connect <url> announced`, or `curl http://relay:4443/announced`, confirms the
relay is reachable and shows what it holds; see [Inspect a relay](/bin/inspect). Connection refused means UDP isn't getting through; certificate
errors on a dev relay want `--connect-tls-insecure` or the `http://`
fingerprint flow.
