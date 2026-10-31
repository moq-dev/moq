# In-tree interop test

Cross-language interop test that builds every client from **this checkout**
and runs them against each other.

This is the in-tree companion to [`test/smoke`](../smoke).
That suite installs each client from its public package registry (crates.io, PyPI,
npm, ...) to catch *packaging* breakage in a release. This one builds each client
from the workspace source (`cargo`, `bun`, `uv`, `cc`) to catch *interop*
regressions before anything is published. No apt/brew/npm/PyPI, and no
distribution-mechanism matrix.

It stands up a `moq-relay`, then for each publisher language publishes an H.264
broadcast and confirms every subscriber sees data flowing before the timeout.
Most subscribers check for a non-empty video frame. The browser additionally verifies
WebCodecs output painted to a canvas and drives the player's pause/resume controls.

Every publisher also sends an Opus tone, one sine per channel, and every subscriber
decodes it. Python and C++ send a mono 440 Hz tone; the rest send 440 Hz left and
660 Hz right, so a channel swap fails too. The Rust CLI imports it from ffmpeg in
fragmented MP4, as both an Opus and an AAC rendition, so AAC import and decode cross
every subscriber (the wire-compat lanes send Opus alone; see
[Released wire compatibility](#released-wire-compatibility)). The browser decodes the picture
and the tone from a looping file through `<moq-publish>`'s file source and encodes both, rather
than capturing Chromium's fake camera and microphone: those run on timers that skip a late tick,
so a runner short of CPU splices the captured tone with no timestamp gap. The Python, Go, and
C++ clients encode it through `moq-ffi` at a 2.5 ms
frame duration, so the matrix covers the FFI audio path with a non-default codec config.
Each subscriber decodes every audio rendition with what it already has: moq-audio for Rust (the
`interop-audio` example, since the CLI has no PCM output), `moq-ffi` for Python,
Go, and C++, `moq-c` for C, `decodebin` on each `audio_N` pad for GStreamer, and
`@moq/hang`'s libav WebCodecs fallback under Node and Bun. Each writes the first
600 ms of PCM and frame timestamps of each rendition, named by its codec, for
[`tone.py`](tone.py), which requires the
publisher's channel count, contiguous timestamps, and each channel's tone carrying
most of its energy by a Goertzel filter. A garbled decode (wrong channel order or
sample rate), silence, a timestamp gap, or a rendition left undecoded fails the cell.
Each publisher's tone starts at timestamp zero, so `--phase` also holds every
rendition's tone to its timestamps within a sample, which fails a decoder that keeps
or drops the wrong Opus pre-skip or AAC priming. It skips the browser publisher, which stamps its file
from its own page clock, the native-JS subscriber, which leaves the pre-skip to the
player as WebCodecs does and stamps milliseconds, and GStreamer's AAC, whose `faad`
decoder reads the priming a frame early. The browser plays each rendition in a page
of its own, the player pinned to it by `controls.audio.target`, and runs the same
filter on each channel of the player's audio graph. A negative control then has the
Rust CLI swap its channels' tones, and every subscriber must refuse every rendition
as the wrong tone, not fail some other way. `interop.sh` is the one place the tones
and each publisher's codecs are chosen.

The normal run also checks a finite raw track through the relay. Its publisher
announces end 4 before writing four 256 KiB groups; the reader verifies every byte,
exactly groups 0 through 3, and a clean end at 4. Rust-to-Rust always runs. Selecting
native JS adds both Rust-to-JS and JS-to-Rust under that runtime, so `--all` covers
Node and Bun. The current CLI also FETCHes group 1 of a finished track from the Rust
publisher and each selected native JS runtime, requires exactly its three objects, each
with its own bytes, and closes under the relay idle-out check.

A late-join lane runs Rust-to-Rust and Rust/Node/Bun in both directions. The publisher
writes the first two objects of group 0 before any subscriber exists and leaves the group
open. The subscriber joins at the latest group, as a player does, and reports once it has
read those two objects; only then does the harness release the rest of the group and
group 1. A join that skips the head cannot pass by racing the publisher. On
moq-transport the subscription starts after Largest Object, so the relay does not replay
the group on it: a subscriber, Rust or JS, and the relay toward its publisher, recover the
head with a joining FETCH before draft 22 and a fill from it.

`--tail` runs the FETCH round, the late joins, and the five finite-tail lanes. The
wire-compat lanes, which swap in released relays and clients, skip them.

The finite clients keep their session alive until the harness acknowledges the
reader's complete clean end over stdin, so a lane tests delivery rather than
shutdown. No sleep stands in for drain completion. A missing group, error, or
stall fails the lane; the timeout only bounds failure. QUIC on localhost rarely
reorders, so the ordering race remains covered by transport unit tests.

The in-tree matrix also has a `finish` case. `just test interop --finish --all`
runs it alone. Readers establish their sessions first, then wait on a private
stdin barrier without subscribing to media. Each publisher imports ten nonempty
H.264 frames in one GOP (the browser encodes the same finite fixture), finishes
all media and its catalog, and obtains the normal broadcast finish handle. Only
then does the runner release readers. They require all ten frames, natural media
and catalog EOF, and the exact broadcast's announcement END. After the finish
handle's real linger expires, the publisher announces a separate untimed health
track. Reading its payload and clean EOF on the original session proves that
withdrawal was not a transport shutdown. The final FIFO acknowledgement permits
teardown only; it never ends the linger. Each publisher also measures monotonic
time from before creating its finish handle and refuses a zero linger or a wait
shorter than that handle promises, without a fixed duration or tolerance.

GStreamer checks the late tail alone. `moqsrc` keeps watching a finished
broadcast, since an announcement means online/offline, so it has no end,
withdrawal, or session to report. It joins once the broadcast has finished and
requires exactly ten nonempty video buffers; its cell passes with the end check
marked N/A. A pinned moq-transport pass records the case as skipped until
[`quest/m2/ietf-finish.md`](../../quest/m2/ietf-finish.md) makes it pass.

Every client closes its session on the way out: publishers stop when their input
ends (the browser on SIGTERM), and subscribers close once they have their frame.
After each publisher's round, the harness waits for the relay to log the close of
every connection the round opened, and fails the round for any connection the relay
timed out instead. A client that stalls and reconnects therefore fails, rather than
passing as a slow cell, even when the idle-out lands after its cell finished. The
check covers runs built from this checkout; the wire-compat lanes, which swap in
released relays and clients, skip it.

Whenever the browser subscriber is in the run, the matrix ends with a close-code
case: Chromium dials the relay with a token its public rules refuse, and
`WebTransport.closed` must carry the relay's code and reason. A WebSocket-only
`Connection` must publish the same Unauthorized session error and stop its
unlimited reconnect loop. Chromium treats a
server's HTTP/3 control stream ending as fatal, so a server that ends it under
the close capsule loses both; no Rust peer is that strict.

`just test media` is a separate, browser-only run that asks a harder
question: is the media a viewer gets actually advancing and in sync, and does the
player survive the publication lifecycle. See [Media QA](#media-qa).

## Clients

| Client | Source under test | Built with | Roles |
|---|---|---|---|
| Rust | `rs/moq-relay` + `rs/moq-cli` (+ `rs/moq-audio` for the `interop-audio` example) | `cargo build` | publish (video + audio) + subscribe |
| Python | `py/moq-rs` (+ `rs/moq-ffi`, import `moq`) | `uv build` wheels (maturin + hatchling), installed into a venv in the run directory | publish (video + audio) + subscribe |
| Go | `go/wrapper` (+ `rs/moq-ffi`, import `moq-go/moq`) | `sh/go/stage.sh` (uniffi-bindgen-go) + `go build` | publish (video + audio) + subscribe |
| C++ | `cpp/moq` (+ `rs/moq-ffi`, `find_package(moq-cpp)`) | `cmake` build + install of `cpp/moq` (uniffi-bindgen-cpp), then `cmake` for the client | publish (video + audio) + subscribe |
| Browser | `js/watch` + `js/publish` | `vite build` + headless Chromium (Playwright) | publish (video + audio) + rendered playback |
| Native JS | `js/net` + `js/hang` + the npm `@moq/web-transport` polyfill | `node` (tsx) and `bun` | subscribe |
| C | `rs/moq-c` | `cargo build -p moq-c` + `cc` | subscribe |
| GStreamer | `rs/moq-gst` (`moqsrc`) | `cargo build -p moq-gst` + `gst-launch-1.0` | subscribe |

The browser, native JS, C, and GStreamer clients subscribe only by choice
(publishing media needs an encoder the native JS runtimes lack, the C client is
intentionally minimal, and `moqsink` publishing needs request-pad muxing this
client doesn't drive). Rust, Python, Go, C++, and the browser publish.

The Go client builds against the modules `sh/go/stage.sh` assembles from
this checkout: `moq-ffi` compiled for the host, bindings regenerated with
`uniffi-bindgen-go`, and the `go/wrapper` module wired to them by a `replace`.
That is the same staging `just go check` uses, so this cell covers the Go
wrapper end to end rather than only compiling it. A shell without
`uniffi-bindgen-go` (the nix devShell ships it) marks the cell unavailable.

The C++ client builds against the package the way an external project would:
`interop.sh` configures, builds, and installs `cpp/moq` into the run directory,
then builds `clients/cpp` with `find_package(moq-cpp)` pointed at that prefix. A
shell without `cmake` or `uniffi-bindgen-cpp` (the nix devShell ships both)
marks the cell unavailable.

The GStreamer client builds the `moqsrc` plugin from `rs/moq-gst` and points
`GST_PLUGIN_PATH` at it, then reads a broadcast with
`gst-launch-1.0 moqsrc ... ! filesink`, decoding each `audio_N` pad through `decodebin`.
The plugin dynamic-links the host's GStreamer, so this cell needs `gst-launch-1.0`
plus the core plugins, `opusdec`, and an AAC decoder (`fdkaacdec`, `avdec_aac`, or
`faad`) on the system.
The `nix develop` shell ships them; a bare shell without GStreamer marks the cell
unavailable rather than failing it.

The `@moq/web-transport` polyfill is the one dependency that comes from npm rather
than this checkout: it's a prebuilt NAPI QUIC/HTTP3 addon, not part of the moq
source tree. Everything else (`@moq/net`, `@moq/hang`, ...) resolves to the
workspace packages, because the JS clients here are bun workspace members.

## Auth

The relay verifies tokens through `moq auth serve` with a key generated for the
run; nothing is anonymous. Each publisher dials with a token for its broadcast's
subtree and each subscriber with one to read it, so every cell also covers the
`?jwt=` URL path in every client.

Clients that print the grant the relay sent back over AUTH, as an
`auth granted publish=[...] subscribe=[...]` line, must report exactly what
their token implies: the Rust CLI (a `moq_net::auth` debug log), the Python,
Go, and C++ clients (read through moq-ffi before they start), and the native
JS subscribers. A cell whose grant is missing or wrong fails even when media
flowed. moq-c's frozen ABI and GStreamer expose no grant, and the browser's
shared connection keeps its session private, so their cells check media alone. AUTH is only on the work-in-progress
moq-lite-07, so the printing clients dial `moq-lite-07-wip` alone and a printing
client that reports nothing never got its grant. The native JS subscribers dial
WebTransport alone and offer it there. The rest keep
their defaults, so the matrix also crosses versions through the relay, which
accepts both.

After the matrix, each publisher whose refusal the harness can read (Rust,
Python, Go, and C++, which log moq-net's error) runs once more with a token that
excludes its broadcast. It must fail loud, logging
Unauthorized and naming the path, and every subscriber must time out. The
browser publisher enforces its grant too, but its driver reports no session
error to read.

Tokens use patterns no prefix could carry, so every cell checks that AUTH\_OK
delivers them as minted: a publisher is granted its exact broadcast, a subscriber
`**/name` (a leading `**` matching zero segments), and the refused publisher
`interop-allowed-*.hang`.

## moq-transport

`--versions` runs the matrix and the tails once per version, against the same
relay. `auto`, the default, is everything above. Any other version pins every
client in its pass to offer that version alone, and `ietf` expands to the even
drafts `moq-transport-18` through `moq-transport-22`. A pinned pass keeps the
media cells, the relay close check, the CLI FETCH round, the late joins, and the
finite tails. The grant, refusal, close-code, and drop cases are moq-lite's and run in
`auto` alone.

The relay logs the version each connection negotiated, so every round in a
pinned pass also fails its `negotiated version` case when any connection it
opened landed on another one, such as a client silently falling back to
moq-lite. The relay names no client, so a fallback fails the round rather than
one cell. The tails check their own connections, so a fallback fails the lane.

The in-tree control lanes also run with `--tail`:

- Four untimed 900-byte datagrams cross the relay in both directions between Rust
  and each native JS runtime. The subscriber acknowledges resolved metadata before
  sending begins, then acknowledges each packet, so this checks framing and bytes
  rather than best-effort burst capacity. The publisher stays open until the read is
  acknowledged, keeping a reliable track end from overtaking the datagrams.
- A JS wire requester gets the Rust publisher's exact largest location through
  TRACK\_STATUS on every draft. Rust also queries the JS publisher's
  non-default priority before subscribing, when the answer can describe a track.
- On drafts 18, 20, and 22, a Rust GOAWAY names a second WebSocket endpoint serving
  the same publisher epoch. The JS reconnect loop retains its reader across the
  move. A separate read through the replacement proves it serves data before the
  original transport is cut; only the replacement can deliver the final group.
  The reverse JS-server-to-Rust-client case uses a marker broadcast excluded from
  the old endpoint's scope, then cuts that endpoint before publishing the final
  group. Both directions use the original publisher stream.

The standalone control endpoints use OS-assigned ports and production QMux,
SETUP codecs, and request sessions. They verify their negotiated version directly.
The datagram and query lanes through moq-relay use its usual version-log check.

Some cases change shape on moq-transport:

- moq-net subscribes there from the largest group, not from a requested range,
  so a tail reader started before the publisher's groups may begin past group 0.
  The tails take any contiguous run of groups through group 3, each verified
  byte for byte, and still require the clean end at 4.
- The AUTH extension carries prefixes alone, and a relay refuses a
  grant narrower than a subtree, closing the session as Unauthorized. Pinned
  passes mint `name/**` for both roles instead of the exact and `**/name`
  patterns above.

The browser pins through the `versions` attribute on `<moq-watch>` and
`<moq-publish>`, and GStreamer through the `versions` property on `moqsrc`.

## Running locally

You need the workspace toolchain on `PATH` (cargo, ffmpeg, bun, uv, go,
uniffi-bindgen-go, cmake, uniffi-bindgen-cpp, a C/C++ compiler). `nix develop` provides
all of it, including the Chromium build matching the workspace's exact npm Playwright pins.

```bash
# Default: rust publishes, rust subscribes (a fast sanity check).
just test interop

# Full matrix: rust/python/go/cpp/browser publish; everyone subscribes.
just test interop --all

# Pick your own axes:
just test interop --publishers rust,python --subscribers rust,c,js-native-bun

# CLI FETCH close, late joins, and finite track tails only: Rust-to-Rust and Rust/Node/Bun in both directions.
just test interop --tail

# The full matrix once per even moq-transport draft (see moq-transport below).
just test interop --all --versions ietf

# Or pick the versions, `auto` being the default moq-lite run.
just test interop --versions auto,moq-transport-18,moq-transport-22

# Subscription termination: Rust/JS response bytes over in-memory transports.
just test bare-fin

# Negative control: no publisher, every subscriber must time out.
just test interop-negative

# Browser-to-browser media output and lifecycle, plus its own negative controls.
just test media
```

Subscriber names: `rust`, `python`, `go`, `cpp`, `js` (browser), `js-native-node`,
`js-native-bun`, `c`, `gst`. Publisher names: `rust`, `python`, `go`, `cpp`, `js`.

A client whose source build fails fails only its own matrix cells (see
`mark_broken` in `interop.sh`); it never aborts the rest of the run.

## Released wire compatibility

`just test wire-compat` compares this checkout with the newest stable,
non-yanked crates.io and npm releases. The existing Interop workflow runs it
nightly and on demand on `main`. Registry installation is omitted from PR runs;
resolver and removed-version regressions run through `just test harness`.

The run records exact releases, npm's resolved lockfile, both CLI help outputs,
and each matrix cell in the harness artifacts. It uses checksummed GitHub
binaries when available on Linux x86\_64, with an exact-version `cargo install`
fallback. Run inside `nix develop` with GitHub CLI registry access.

Four sources sign JWTs: current and published Rust CLI, and current and
published `@moq/auth`. Both Rust CLIs and both JavaScript packages verify every
token and assert its normalized root and permission scope. Current and released
`hang` and `@moq/hang` encode and decode one another's catalogs and legacy frame
headers, checking the exact timestamp and payload. These are format checks,
independent of a media decoder.

The session lanes take the moq-lite drafts from each CLI's `--connect-version`
choices. IETF drafts are left out on purpose: our clients and relays always
prefer moq-lite with each other. `just test interop --versions ietf` covers
moq-transport within this checkout (see [moq-transport](#moq-transport)). The relay
offers only the cell's version, and JavaScript checks the negotiated version.
The relay is anonymous here, since released binaries may predate AUTH; the
default matrix covers tokens and grants.
Current Rust media publishers feed released Rust and JS readers, then released
publishers feed current readers, through both relay sources. Each reader is its
own lane, `media-rust` and `media-js-native-node`. Each Rust CLI publishes the
stereo interop tone from fragmented MP4 (released CLIs publish it as CMAF) as
Opus alone, since the released `@moq/hang`'s libav fallback has no AAC decoder, and
every reader must decode both a video frame and the tone, which `tone.py`
judges as in the matrix: a garbled decode, a missing audio rendition, or a
timestamp gap fails the lane, and the swapped-tone control must fail it. Rust
exports decode through ffmpeg, which also logs each decoded audio frame's
timestamp. The JS reader is `compat/client.ts`, not the interop subscriber,
whose API follows the checkout. One copy runs against each package set, using
only API both have: it reconstructs the catalog, decodes the container, and
decodes audio through `@moq/hang`'s libav fallback. It copies samples in the
decoder's own layout, since the released fallback garbles every channel past
the first when converting to planar. The FETCH lanes keep a video-only publisher.
Before cross-version sessions, current and released media controls each run
with their own relay, publisher, and Rust/JS readers on moq-lite-06. These
controls always run, including when a planned break skips cross-version lanes,
so a broken reader cannot make compatibility failures ambiguous.

Each `moq-net` library states whether a draft has FETCH (lite-05 onward
today). A draft without it in both is logged and its FETCH lanes skipped; one
the release supports and the checkout dropped fails, unless a planned break covers
the `fetch-capability` cell for that version. Both Rust versions FETCH the group the
publisher's own compat client observed and compare every frame's exact
payload. JS publishers write one group after that subscriber's demand, and the
opposite-source Rust CLI FETCHes it by the same observed ID.

Every session cell runs, and the run counts the cells that ran and were
skipped, then lists each failing cell before it fails.
Checkout-only versions are logged and omitted. Removing a released version
fails before sessions start. A maintainer-approved break belongs in
`compat/planned-breaks.json` with its reason and the exact affected release
versions. Keyed by a protocol name it skips every cell of that version, and
excuses the checkout removing it; with `cells` it skips only the named lanes,
optionally narrowed to versions, a relay source, or a publisher source. Every
skipped cell is logged with the break's name. Entries become errors once an
affected release changes or no longer offers a listed version, forcing a
re-measure. Future WIP drafts receive no automatic exception.

`just test wire-compat --remeasure` is that re-measure, run after each release
publishes (see `CONTRIBUTING.md`). It runs the cells planned breaks cover
without letting them fail the run, then rewrites `compat/planned-breaks.json`:
a break with a failing cell is re-pinned to the releases just measured, and one
with none is deleted. It prints each break's failing cells. Review the diff and
refresh each reason before committing.

## Media QA

The matrix asks "did bytes arrive and did a pixel light up". That passes on a
frozen picture, on silence, and on audio a second out of step, so
`just test media` measures the media itself, browser to browser.
Use `just test media --media-engines chromium,firefox,webkit` to run each pinned
Playwright player against a Chromium publisher, and `--media-publisher firefox` to
publish from Firefox instead. Chromium comes from the root devShell;
the dedicated Firefox CI job provides its other engines. The harness runs the same
lifecycle cases, content tolerances, and five negative controls per engine. A missing runtime or required API fails that engine by name.
Firefox and WebKit pages explicitly use WebSocket; Chromium keeps normal
transport selection. This does not measure Firefox's QUIC path.

The capability report names the engine version and separately lists missing
publisher and player APIs. Capabilities are measured on every run rather than assumed
from an engine name. A publisher needs one of the capture paths `@moq/publish` tries,
in order: a window `MediaStreamTrackProcessor` (Chromium), one in a worker the track
is transferred to (Safari), or a `<video>` element sampled per presented frame.
Firefox 157 in Playwright 1.64 has neither `MediaStreamTrackProcessor` nor a
transferable `MediaStreamTrack`, so a Firefox publisher takes the `<video>`
fallback. The fixture must log that fallback exactly when the probe predicts it,
or the run fails `capture path`. No Playwright engine on Linux takes the worker
path. Playwright WebKit is not Safari; Safari device/transport
bugs and real Firefox transport negotiation still require manual evidence.

A Firefox publisher does not pass yet, so no CI job runs one. Under load its runs
fail audio/video sync by about one 200 ms tone step (audio ahead of the picture),
and late joins land 1.8 s behind live against the 1.5 s bound. Chromium forced onto
the same `<video>` fallback stays in sync, so the cause is Firefox's. The capture
case also fails, since Firefox has no fake camera configured.

Firefox only resumes Web Audio on an output device, so each Linux run gets a
private null sink from `test/lib/pulse.py`. That sink renders in 50 ms blocks, so a fresh
player's context starts in tens of milliseconds, as on a real device; the null sink's
default 2 s block held a late joiner's audio, and with it live playback, for up to 2 s.
Chromium plays to its fake output instead,
with or without a host sound server.

Nightly runs the Firefox player only. Playwright's Linux WebKit 27.2 fails two
cases: its VP8 decoder keeps reporting the first coded size after an in-band
resize, failing coded-size growth, and detaching logs a WebSocket "Connection
reset by peer" console error that is not yet root-caused.

The publisher is a fixture, not a fake camera: a canvas painting a frame counter
as black/white blocks, and a tone stepping through a fixed frequency table. Both
are indexed off one `AudioContext` clock, so the subscriber can read the frame it
is presenting off the canvas, read the tone step off the player's own audio
graph, and compare them. **Everything measured is browser output.** Nothing here
observes a physical speaker or display; a run says the player emitted the right
samples, not that a machine played them.

Each run covers, against a real local relay:

- **capabilities** - probes native APIs separately for the publisher and player, and fails
  naming what is missing rather than skipping a case.
- **cold start** - the publisher reports when it is announced and encoding, then
  a fresh page joins. No reload, unlike the matrix driver: a subscriber that
  needs a second page load is an initialization bug, not a race.
- **user gesture** - Chromium is launched with
  `--autoplay-policy=document-user-activation-required`, which applies to
  top-level Web Audio contexts. The fixture and player graphs must be suspended
  before either page is clicked, then both must carry audio afterwards. Firefox
  is launched with its Web Audio autoplay block enabled. Playwright's usual page
  reads grant activation and would invalidate this assertion, so the player logs
  each sample to the console and the harness never evaluates in a player page
  before its click. The Chromium fixture is read over CDP with
  `userGesture: false`, and capability probes run in a separate page.
- **capture permission** - the camera case uses a fake device for deterministic
  input, while Playwright denies and then grants permissions. Both source errors
  must be visible; neither a full nor a microphone-only denial may announce a
  broadcast. Granting both permissions must recover and encode without reload.
  The fake device is not physical hardware, and the headless permission decision
  is not a person clicking a browser prompt.
- **pause and resume**, **unsubscribe and rejoin**, **detach and reattach**,
  **publisher stop and same-path republish**, and **late join**. The late
  joiner may first show the GOP from before the demand gap, but it must present
  a frame within 5s of loading and then play, with the presented frame moving,
  within 1.5s of the fixture's painted frame within 2s, both bounds about twice
  the worst seen in looped runs under CPU load. A rejoin and a reattach cross
  the same demand gap, so they must reach live the same way.
- **codec identity and coded-size changes** - an AVC-to-VP8 change keeps the
  broadcast and rendition ID while moving to a fresh track. Playback must keep
  advancing and in sync through the change. A VP8 resize doubles the presented
  dimensions on the same track. The pixels carry a source generation, so old
  pictures cannot reappear after the new generation starts presenting.
- **resources return to baseline** - the page wraps `WebTransport`, `WebSocket`,
  `AudioContext`, and `Worker` to count live instances, so a detach that leaks a
  session is visible rather than merely invisible.

Tolerances come from the fixture: video must present at half the fixture's 30fps
or better and never go backwards, the tone must stand 15dB above the spectrum's
median, and audio/video skew must stay within one 200ms tone step for 90% of
samples. One step is the floor set by the analyser window straddling a step
boundary and the canvas holding a frame up to one frame old.

Each window opens once the player's audio buffer has filled and plays. Until
then the player holds its first decoded frame, silent, for its sync delay, which
rises past 400ms under load; that is startup, not a gap in playback.

Run a focused lifecycle case with `just test media --media-cases rename`.
It uses the same real fixture, player and content oracles while leaving cold-start
grading and the unrelated negative controls to the default full run. A focused
pass makes no claim about those omitted checks.

`just test media --media-cases replacement` keeps publisher A open while B takes
the same path, then closes B and follows A again. Lite-07, lite-06, and IETF draft
22 viewers must present each run's marked pixels and synchronized audio, with
no A pictures spliced into B. Epoch-aware viewers also check the broadcast
identities. Both publishers use lite-07. Each switch must present within a bound
derived from its parts, counted from B's readiness (connected, with catalogs) or
B's withdrawal: the relay's 300 ms announcement hold, five loopback round trips
at QUIC's 1 ms timer granularity, the 500 ms GOP, one frame to decode, the 50 ms
playback delay, and one page sample plus one driver poll. Publisher preparation
is not part of it.

The CLI integration suite's `epochs` cases exercise the real QUIC relay with
lite-07, lite-06, and IETF draft 22. Held catalog snapshots and media
subscriptions keep A while fresh catalog handles resolve B with fresh sibling
pins. Later metadata and FETCH work cannot join B through A's retired handle;
withdrawing B reveals the still-live A to fresh readers. Announcement followers
must see each switch within the relay's and the reader's announcement holds plus
one round trip.

The run ends with negative controls. Each injects a defect and names the
assertion that has to catch it, and passes only by failing there:

| Control | Must fail |
|---|---|
| tone muted at the source | `audio tone` |
| picture frozen after the first frame | `video progress` |
| tone table shifted 800ms ahead of the picture | `audio/video sync` |
| the detached player's session never torn down | `resource baseline` |
| the latecomer's player delayed 3s behind live | `late join reaches live` |

The leaked-session control waits for an extra `AudioContext` rather than an extra
session: every player on one relay URL shares a transport, so a session count
cannot move.

Not covered yet: publishing from engines other than Chromium in CI, the worker
capture path, and any claim about physical playback.

The relay's port is reserved for the run rather than fixed, so two checkouts can
run the interop test at once; `INTEROP_PORT` pins one instead. A failing run keeps its
directory, including a Playwright trace of the failing page, and CI uploads it;
`HARNESS_KEEP=1` keeps a passing run's too. See [the harness
contract](../README.md).

## Layout

```text
interop.sh              orchestrator: build clients, run the relay + matrix or media checks
interop.toml            relay config (token auth via `moq auth serve`, self-signed localhost)
report.py               the CI report: case records to a static pass/fail page
tone.py                 judges a subscriber's decoded audio against its publisher's tones
bare-fin.ts             the JS side of `just test bare-fin`, driven by moq-net's tests
varint.ts               the JS side of the varint check, driven by moq-net's tests
clients/
  rust/audio.rs           the Rust subscriber's audio decode via rs/moq-audio (`interop-audio`)
  rust/tail.rs            the finite raw-track client (`interop-tail`)
  rust/catalog.rs         the compressed-catalog publisher (`interop-catalog`)
  python/interop.py       publish/subscribe via py/moq-rs (import moq)
  go/main.go              publish/subscribe via go/wrapper (import moq-go/moq)
  cpp/main.cpp            publish/subscribe via cpp/moq (find_package(moq-cpp))
  js/                     headless-Chromium publish/subscribe via @moq/watch + @moq/publish
    driver.ts             the interop matrix's browser publisher/subscriber
    close.ts              the refused session's close code and reason
    media.ts              the media output + lifecycle checks
    harness.ts            shared Playwright plumbing
    src/contract.ts       what the page and its drivers agree on, free of browser imports
    src/fixture.ts        the deterministic publisher (frame counter + stepped tone)
    src/pattern.ts        how that fixture encodes itself into the picture and the audio
    src/probe.ts          subscriber-side measurement, taken at the sinks
    src/instrument.ts     live counts of the platform resources the page holds
    src/close.ts          the refused session, read off `WebTransport.closed`
  js-native/subscribe.ts  subscribe via @moq/net + @moq/hang + the WebTransport polyfill
  js-native/catalog.ts    the compressed-catalog reader, pinned to one version
  c/subscribe.c           subscribe via rs/moq-c
```

## CI

`.github/workflows/interop.yml` runs the full matrix nightly (and on demand, and on
release PRs that touch `test/interop/`), then again with `--versions ietf`. A
release PR's moq-transport step runs only drafts 18 and 22 between Rust and native
JS. A red cell means a real interop break in the current tree.

The scheduled run on `main` publishes its report to <https://git.moq.dev/interop/>;
every other run keeps it as the `interop-report` artifact. The report is a pass/fail
table of publisher by subscriber for each suite, case, transport, and version, plus each
workflow step's outcome. A red cell links to its run and to the failure artifact, and
its tooltip names the log inside that artifact.

The report reads records, never logs. With `HARNESS_RESULTS` set, `interop.sh` and
`compat.sh` append one JSON line per case through `harness_result` (`test/lib/harness.sh`):
suite, case, publisher, subscriber, transport, version, outcome, seconds, detail, and
log. A run first records every case it will reach as `not run`, before building
anything, so a harness that dies in setup or mid-suite leaves red `not run` cells
rather than missing ones. Transport and version are `auto` where each client negotiates
its own; a pinned pass, the drop lanes, and wire-compat record their version. `report.py` renders the page:

```bash
HARNESS_RESULTS=/tmp/results.jsonl just test interop
just test interop-report /tmp/results.jsonl /tmp/interop-report
```

## Subscription termination

`just test bare-fin` exchanges encoded subscription responses between Rust and
JS, then feeds them into each implementation's subscriber over an in-memory
transport. It checks bare FIN before and after SUBSCRIBE\_START on lite-05/06/07,
and FIN without PUBLISH\_DONE on IETF draft-18. Clean-end controls use the same
path. This tests response interoperability, not network delivery or relay behavior.
The interop workflow runs it alongside the real-transport matrix.

## Varints

Every `just test interop` run starts with `varint_interop` in moq-net, which
hands moq-net's QUIC and leading-ones encodings of each varint size boundary
(plus 2^53, where a JS `number` stops being exact, and 2^62 - 1) to
`varint.ts`. That script decodes them into js/net's `U64`, checks its
`number` conversion, and returns js/net's own encodings, which Rust requires to
match byte for byte and decode back to the same value.

`lite_auth_unsupported_interop` sends an AUTH\_OK followed by AUTH\_ERROR
NOT\_SUPPORTED across Rust and JS in both directions, checking that the later
refusal retains its meaning and both bindings emit the same bytes.

`lite_tracks_interop` checks moq-lite-07's Tracks Stream both ways: `tracks.ts`
answers a Rust TRACKS request with js/net's publisher, whose listing (each
TRACKS\_ADD with its TRACK\_INFO, SYNCED, a TRACKS\_REMOVE, then the FIN when
the broadcast ends) Rust decodes, and it re-encodes Rust's listing events byte for
byte.

`lite_varint_interop` runs next to it and does the same through moq-lite's
version dispatch: `lite-varint.ts` decodes Rust's lite-06 (QUIC) and lite-07
(leading-ones) varints, a SETUP carrying a 62-bit Hop ID, a datagram, and a
GROUP stream with frames, and re-encodes them byte for byte. Past 2^62-1 the
range is per version: JS writes lite-07's 64-bit values, which Rust reads back,
and JS refuses them on lite-06.

`ietf_datagram_interop` does the same for moq-transport's `OBJECT_DATAGRAM` on
each supported draft: `ietf-datagram.ts` decodes Rust's datagrams and their
Timestamps and re-encodes them byte for byte, and Rust decodes the datagram the
JS publisher sends.

`ietf_track_status_interop` sends Rust's `TRACK_STATUS` to the JS publisher on
each supported draft through `track-status.ts`, and Rust decodes each
`TRACK_STATUS_OK` and the refusal for a missing broadcast the way its
subscriber reads them.

The finite-tail matrix also publishes compressed tracks from Rust and native JS through
the relay, in both directions on lite-07 and lite-06. It checks multiple frames in each
DEFLATE window, empty frames, the plain fallback and the received track's codec flag.

The compressed-catalog lane then fans one Rust hang catalog out to three native JS
readers at once, each pinned to lite-07, lite-06, or moq-transport-22 and reading it as
`@moq/watch` does. Only the lite-07 reader sees the flag; the other two share the relay's
inflated copy, so `moq_relay_inflated_bytes_total` must grow by exactly the catalog's
inflated size. The harness enables the relay's `[stats]` and `[internal]` listener for it.
