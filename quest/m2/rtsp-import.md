# [M] RTSP import

## Goal

`moq import rtsp --connect rtsp://camera/stream` publishes an IP camera's
H.264 or H.265 video and AAC audio as a broadcast. It runs on the camera's
own network, so a camera behind NAT needs no inbound port. A `moq-rtsp`
library crate carries it, so other publishers embed the ingest instead of
shelling out to the CLI.

The library's entry point ingests one RTSP session into a
`broadcast::Producer` the caller supplies, as moq-mux's importers take one,
with no reconnect; importer state stays private. It returns when the session
ends, fails, or its timeline jumps. The CLI wraps it with reconnect and a
fresh epoch per run. A caller with its own supervisor, such as Pronto's
truck, runs the library directly, including two sessions (two URLs) into one
broadcast.

Non-goals: an RTSP server (push ingest), a relay pulling cameras itself, and
codecs nothing in the stack plays (G.711, MJPEG).

## Plan

Moved from m3 to m2 on 2026-10-05: security cameras are moq.pro's priority
customer segment, so the "no consumer" reason for parking it is gone. moq.pro's
camera guide and Pronto's truck consume it.

Decided 2026-10-01:

- retina is the RTSP client and depacketizer. RTP over TCP interleaved is
  the default: it crosses NAT and firewalls with the session it already has.
- Video passes through without re-encoding into moq-mux's H.264 and H.265
  importers. Ask retina for Annex-B framing so parameter sets arrive in-band
  with each keyframe (cameras often send them only in the SDP), and publish
  nothing until the first keyframe.
- AAC maps to moq-mux's AAC importer. Audio the stack cannot play, G.711
  above all (common on cameras), is skipped with one clear warning and the
  broadcast goes on video-only. Transcoding it waits for a customer who
  needs it.
- A broadcast name always means the same content, so a timeline restart is
  never spliced onto the old broadcast. A new session's RTP time restarts,
  and a timestamp jump inside one session (retina #64) ends that library
  session and returns to its caller instead of failing the track. The caller
  owns the broadcast: it finishes it cleanly and restarts every session
  sharing it under a fresh epoch, per
  [Broadcast epochs](/quest/m0/broadcast-epoch/README.md), with a fresh
  catalog, tracks, and importers starting on a keyframe. The CLI publishes
  each run (each reconnect) under a fresh epoch. Decided 2026-10-02 and
  2026-10-05; shifting timestamps onto the existing catalog clock behind a
  `discontinuity()` marker was rejected.
- Credentials ride the URL's userinfo, as every RTSP tool takes them. retina
  refuses a URL with userinfo, so strip it into `SessionOptions::creds`
  before `describe`, and log only the redacted URL (`RedactedUrl`). A
  `--connect` URL is visible to other local users through `/proc` cmdline;
  accepted for now, and revisit with an env var or file for shared hosts.
- Prior art: moq.pro's `pronto/truck/src/camera.rs` is a working retina pull
  client, video only, run against mediamtx and real cameras.

Decided 2026-10-05:

- The library ingests one session and never reconnects. The caller supplies
  the `broadcast::Producer` and importer state stays private, so two
  sessions, such as the truck's H.264 SD and H.265 HD URLs, can publish
  renditions of one broadcast. Two sessions on one catalog need
  [Shared import clock](/quest/m1/shared-clock.md) to land on one timeline.
  A session end, a failure, and a timeline jump all return to the caller,
  which decides whether to retry. Reason: the truck deliberately exits
  for systemd so a stuck Starlink session surfaces, and an in-process retry
  inside the library would force it to opt out.
- `moq import rtsp` owns reconnect with backoff: each new session gets a fresh
  epoch, a fresh catalog, and starts on a keyframe, as above. Rejected: the
  truck adopting the crate's reconnect.

Test against a local RTSP source serving a known H.264 + AAC clip (Pronto
uses mediamtx), checking the catalog and first frames; a G.711 source that
publishes video only; two sessions ingested into one caller broadcast, where
one session's timestamp jump has the test caller restart both under a new
epoch; and a source restarted mid-stream, whose RTP time resets: the library
call returns, and the CLI ends the first broadcast cleanly and publishes a second one under
a new epoch, starting on a keyframe with its own catalog.
`doc/bin/cli.md` documents `import rtsp`.

Public API: the `moq-rtsp` crate and the `moq import rtsp` subcommand.
Wire: none. Both are additive, so it is backported to `release` once it
lands on main.

The moq.pro guide link below resolves once moq.pro#2210 merges.

## Required

- [Shared import clock](/quest/m1/shared-clock.md) - two sessions importing into one caller broadcast share one timeline

## Related

- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - the fresh epoch a caller and the CLI mint for each restarted broadcast
- [Import at the first frame](/quest/m1/import-first-frame.md) - a lone session's catalog publishes at its first frame
- [moq.pro's Pronto truck](https://github.com/moq-dev/moq.pro/blob/main/pronto/truck/src/camera.rs) - the prior art this generalizes
- [moq.pro: Pronto truck on moq-rtsp](https://github.com/moq-dev/moq.pro/blob/main/quest/m3/truck-rtsp.md) - runs the library entry point under its own supervisor
- [moq.pro: Camera (RTSP) guide](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/rtsp-guide.md) - documents `moq import rtsp` for customers once a release ships it
