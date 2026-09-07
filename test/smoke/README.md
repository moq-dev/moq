# In-tree smoke test

Cross-language interop smoke test that builds every client from **this checkout**
and runs them against each other.

This is the in-tree companion to [moq-dev/smoke](https://github.com/moq-dev/smoke).
That repo installs each client from its public package registry (crates.io, PyPI,
npm, ...) to catch *packaging* breakage in a release. This one builds each client
from the workspace source (`cargo`, `bun`, `uv`, `cc`) to catch *interop*
regressions before anything is published. No apt/brew/npm/PyPI, and no
distribution-mechanism matrix.

It stands up a `moq-relay`, then for each publisher language publishes an H.264
broadcast and confirms every subscriber sees data flowing before the timeout.
Most subscribers check for a non-empty frame. The browser additionally verifies
WebCodecs output painted to a canvas and drives the player's pause/resume controls.
The browser publisher also sends fake microphone audio, which the browser
subscriber checks end-to-end.

## Clients

| Client | Source under test | Built with | Roles |
|---|---|---|---|
| Rust | `rs/moq-relay` + `rs/moq-cli` | `cargo build` | publish + subscribe |
| Python | `py/moq-rs` (+ `rs/moq-ffi`, import `moq`) | `just py build` (maturin editable into `.venv`) | publish + subscribe |
| Go | `go/wrapper` (+ `rs/moq-ffi`, import `moq-go/moq`) | `go/scripts/stage.sh` (uniffi-bindgen-go) + `go build` | publish + subscribe |
| Browser | `js/watch` + `js/publish` | `vite build` + headless Chromium (Playwright) | audio/video publish + rendered playback |
| Native JS | `js/net` + `js/hang` + the npm `@moq/web-transport` polyfill | `node` (tsx) and `bun` | subscribe |
| C | `rs/libmoq` | `cargo build -p libmoq` + `cc` | subscribe |
| GStreamer | `rs/moq-gst` (`moqsrc`) | `cargo build -p moq-gst` + `gst-launch-1.0` | subscribe |

The browser, native JS, C, and GStreamer clients subscribe only by choice
(publishing media needs an encoder the native JS runtimes lack, the C client is
intentionally minimal, and `moqsink` publishing needs request-pad muxing this
client doesn't drive). Rust, Python, Go, and the browser publish.

The Go client builds against the modules `go/scripts/stage.sh` assembles from
this checkout: `moq-ffi` compiled for the host, bindings regenerated with
`uniffi-bindgen-go`, and the `go/wrapper` module wired to them by a `replace`.
That is the same staging `just go check` uses, so this cell covers the Go
wrapper end to end rather than only compiling it. A shell without
`uniffi-bindgen-go` (the nix devShell ships it) marks the cell unavailable.

The GStreamer client builds the `moqsrc` plugin from `rs/moq-gst` and points
`GST_PLUGIN_PATH` at it, then reads a broadcast with
`gst-launch-1.0 moqsrc ... ! filesink`. The plugin dynamic-links the host's
GStreamer, so this cell needs `gst-launch-1.0` + the core plugins on the system.
The `nix develop` shell ships them; a bare shell without GStreamer marks the cell
unavailable rather than failing it.

The `@moq/web-transport` polyfill is the one dependency that comes from npm rather
than this checkout: it's a prebuilt NAPI QUIC/HTTP3 addon, not part of the moq
source tree. Everything else (`@moq/net`, `@moq/hang`, ...) resolves to the
workspace packages, because the JS clients here are bun workspace members.

## Running locally

You need the workspace toolchain on `PATH` (cargo, ffmpeg, bun, uv, go,
uniffi-bindgen-go, a C compiler). `nix develop` provides all of it except
Playwright's Chromium, which
`smoke.sh` fetches on first run (`bunx playwright install chromium`).

```bash
# Default: rust publishes, rust subscribes (a fast sanity check).
just test smoke

# Full matrix: rust/python/go/browser publish; everyone subscribes.
just test smoke-full

# Pick your own axes:
just test smoke --publishers rust,python --subscribers rust,c,js-native-bun

# Negative control: no publisher, every subscriber must time out.
just test smoke-negative
```

Subscriber names: `rust`, `python`, `go`, `js` (browser), `js-native-node`,
`js-native-bun`, `c`, `gst`. Publisher names: `rust`, `python`, `go`, `js`.

A client whose source build fails fails only its own matrix cells (see
`mark_broken` in `smoke.sh`); it never aborts the rest of the run.

## Layout

```text
smoke.sh                  orchestrator: build clients, run the relay + matrix
smoke.toml                relay config (anonymous, self-signed localhost)
clients/
  python/smoke.py         publish/subscribe via py/moq-rs (import moq)
  go/main.go              publish/subscribe via go/wrapper (import moq-go/moq)
  js/                      headless-Chromium publish/subscribe via @moq/watch + @moq/publish
  js-native/subscribe.ts  subscribe via @moq/net + @moq/hang + the WebTransport polyfill
  c/subscribe.c           subscribe via rs/libmoq
```

## CI

`.github/workflows/smoke.yml` runs the full matrix nightly (and on demand, and on
PRs that touch `test/smoke/`). A red cell means a real interop break in the
current tree.
