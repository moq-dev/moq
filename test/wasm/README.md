# @moq/wasm browser harness

Runs the browser bindings ([`rs/moq-wasm`](../../rs/moq-wasm) -> `@moq/wasm`) in
headless Chromium against a real `moq-relay`, over real WebTransport.

It exists because nothing else can see this code. The crate root is
`#![cfg(target_arch = "wasm32")]`, so a host-target `cargo check --workspace`
compiles it to nothing; `just rs wasm` compiles it for wasm32 but says nothing
about behavior. That gap shipped [#2811](https://github.com/moq-dev/moq/issues/2811)
on `main`: `ClientBuilder` never called `with_protocols`, the browser negotiated
no WebTransport subprotocol, and moq-net rejected the empty string as an unknown
ALPN. The bindings could not open a session at all and every gate stayed green.

## Running

```bash
just test wasm
```

```bash
just test wasm --timeout 60
```

Everything is built from this checkout: `just js wasm` for the bindings, `cargo
build -p moq-relay` for the relay. Each relay reserves its own port (see
[the harness contract](../README.md)); `WASM_PORT` pins the first one instead.
`WASM_PROFILE` picks the relay's cargo profile, and `RELAY_BIN` points at a
prebuilt relay instead.

A failing run keeps its run directory, including a Playwright trace of the
failing page, and CI uploads it; a passing run deletes its own. See
[the harness contract](../README.md).

## Rust clock

`moq-clock`'s browser backend also runs here, using `wasm-bindgen-test` and the
same Chromium supplied by the root devShell:

```bash
bun install --frozen-lockfile
just rs wasm-test
```

The suite covers monotonic time, sleep deadlines, timeouts, an interval's immediate
first tick, and a controlled `Date.now()` reading. `rust.ts` opens wasm-bindgen's
harness page and propagates failures through Cargo and `just`.

## Shape

`run.sh` builds, starts the relays, and hands their URLs to `driver.ts`, which
opens one Chromium tab on the bundled page and evaluates the suite in
`src/main.ts`. Each case reports back as data; the driver prints it and sets the
exit code.

The publisher is `@moq/net` (the hand-written TypeScript implementation) and the
subscriber is `@moq/wasm` (the Rust one compiled to WebAssembly), so every case
is also a cross-implementation interop check.

The page loads `js/wasm/dist/moq.js` from a URL rather than an import the
bundler can follow, so what runs is the generated output exactly as it ships:
the glue, the `.wasm` it fetches, and the JS names `#[wasm_bindgen]` chose.
`just check` and `run.sh` both type-check the harness against the generated
`moq.d.ts`. That is worth doing: wasm-bindgen resolves a type in a signature
by its Rust identifier alone, so a binding can compile, run, and still publish
typings that name the wrong class.

### Relays

One per protocol flavour, since negotiation is the part that broke:

| name    | relay                            | negotiates                             |
| ------- | -------------------------------- | -------------------------------------- |
| `lite`  | defaults                         | `moq-lite-06`, over its own ALPN   |
| `ietf`  | `--listen-version moq-transport-22` | `moq-transport-22`, over its own ALPN  |
| `setup` | `--listen-version moq-lite-02`   | the `moql` ALPN, version chosen by SETUP |

The `setup` relay is as close as a real relay gets to the SETUP fallback path.
The branch that maps the browser's empty `protocol()` to `None` needs a server
that selects no subprotocol at all, which moq-relay never does, so it stays
uncovered here.

### Cases

- **negotiates the relay's version** -- the #2811 regression. Deleting the
  `with_protocols` call fails this on `lite` (it falls back to `moq-lite-02`
  over SETUP) and `ietf` (nothing left to negotiate).
- **reads a published broadcast** -- announce, subscribe, and read two
  consecutive groups, checking every frame byte for byte. One frame is 128 KiB,
  which is what exercises the chunked read path in the transport adapter. The
  reader asks for each next group, so the real-time subscription never drops one
  superseded mid-read, which a timer-paced publisher on a loaded runner did.
- **refuses a track the publisher does not serve** -- the refusal has to
  surface as an error, from `subscribe` (lite-06, which looks a track's info up
  first) or from `recvGroup` (IETF and lite-02 can accept first). A group or a
  clean end fails, and a hang fails on the case timeout.
- **free() rejects a pending consume** -- freeing a session closes it, so a
  `consume` still waiting on an unannounced path has to reject. A fresh session
  then has to connect, which a corrupted wasm heap would not.
- **free() cancels a pending read** -- freeing a `Broadcast`, `Track`, or
  `Group` while its call is pending rejects the call, even when data is already
  buffered, and the page keeps working. An `async fn(&self)` binding fails both
  cases, since wasm-bindgen keeps `&self` borrowed across the await.

### Known failures

A case can name relay flavours where it is expected to fail, with the issue that
tracks why and a substring the failure has to contain. A matching failure prints
as `known` and does not fail the run. Anything else does: a failure that doesn't
match the signature is reported as the new regression it is, and so is a case
that starts passing, so the marker has to be removed with the fix.

The signature is the point. Excusing a case by name alone would retire its
coverage, since an unrelated break in `consume`, `subscribe`, or frame copying
would land green under the same marker.

## Not covered

The publish direction: `moq-wasm` binds the consume path only. When
[#2814](https://github.com/moq-dev/moq/pull/2814) lands, the fixture publisher
here becomes a second wasm session and the interop runs both ways.

Firefox. Playwright can launch it and it opens WebTransport sessions here, but
it ships no `WebTransport.prototype.protocol`, so it cannot request or read a
subprotocol. Run against Firefox 153, the `lite` relay negotiates `moq-lite-02`
over SETUP instead of `moq-lite-06` and the `ietf` relay rejects the connection
outright: four of the nine cases fail, including the version-negotiation case
this harness exists for. There is nothing left to assert about negotiation, so
Firefox joins the matrix when Gecko implements the subprotocol, not before.
`@moq/net` sidesteps this in the browser by routing Firefox below 153 to
WebSocket; `@moq/wasm` has no such fallback.

## Backend baseline

```bash
just test wasm-baseline
```

Runs the existing async consumer correctness cases first, then retains raw
handwritten JS and Rust WASM consumer samples against the same real relays.
Publisher and subscriber pages are separate. Publisher count and subscribers
per publisher vary independently; payload sizes cover small audio-shaped
frames through 128 KiB. Alternating and consecutive A/A runs expose variability.
Every payload byte and group sequence is checked. The JS fallback runs with
WebTransport disabled in its consumer page; WASM's missing fallback is an
explicit gap, not a simulated result.

`backend.json` in the retained harness run includes complete backend entrypoint
assets and their gzip sizes, tool/source/browser/hardware provenance, raw
module and first-frame times, consumer renderer CPU, and heap/WASM capacities.
The neutral harness is listed separately. CPU uses CDP `threadTicks` and includes
connection setup, protocol processing, binding copies and byte validation.
Network waiting is reported as elapsed time, not CPU. Current bindings cannot
attribute protocol and boundary CPU separately or count copies and GC; the
report states those limits. Fresh contexts and no-store responses mean uncached
page imports, with browser process and OS caches still warm.

No timing threshold fails a shared runner. Bad bytes, negotiation, incomplete
samples or an unavailable claimed CPU clock fail. Retain raw rounds and compare
their range before drawing a performance conclusion. Nightly uploads the run
for 45 days. This is a consume-path baseline, not approval to adopt WASM for the
complete JS API.

The independent integer fixtures retain decimal strings through `u64::MAX`.
Leading-ones and QUIC encodings round-trip legal values exactly and refuse each
format's overflow. Real group tests replay the safe 31-bit, 32-bit and near-53-bit
boundaries from the original async cases in
[#5337](https://github.com/moq-dev/moq/issues/5337), plus forced WASM memory
growth. That contribution pins original async consumer source
`bd2840874627d70fd5d33dddacec9ea5d77a7004`; its publisher prototype/Charon and
WASM-to-JS measurements are separate, unverified results here. Full network
sequences above 2^53 need a native publisher and a JS model that can represent
them before adoption. WASM frame timestamps and complete publish/backpressure
API parity remain explicit gaps too.

## Generated bindings

```bash
just test wasm-baseline --uniffi
```

`--uniffi` adds moq-ffi, bound by UniFFI over ubrn's `wasm2` player, as a third
baseline backend. It also runs parity cases and per-call timings in `uniffi/` against
every relay, once normally and once under a strict CSP. It builds from a scratch
checkout, since ubrn needs a UniFFI pin that does not land.
[generator.md](generator.md) is the report that chose this binding.

### Parity with `@moq/net`

What `@moq/net`'s in-tree consumers (`js/hang`, `watch`, `publish`, `json`, `msf`,
`e2ee`, `room`, `moq-boy`) use, against moq-ffi on wasm32. The model is largely there:
announcements and `follow`, broadcasts, tracks and their demand, subscriptions and their
updates, ordered groups, timed frames, datagrams, the bandwidth allocator, and
cancellation. `uniffi/cases.ts` exercises the parts listed in its cases.

Still missing or partial, by what the consumer does:

| `@moq/net` | used by | moq-ffi on wasm32 |
| --- | --- | --- |
| `Connection` reconnects with backoff, and re-dials on a new `url` or `versions` | watch, publish, moq-boy | dials once; `backoff` is `Unsupported` |
| WebSocket fallback when WebTransport is missing | watch, publish | WebTransport only; `websocket` is `Unsupported` |
| `connection.probe` round trip and receive rate | watch, room | `stats()` reports little in the browser |
| `group.closed`, `group.arrived`, `group.expired` | hang | none on `MoqGroupConsumer` |
| frame `arrived` time | hang | `MoqFrame` carries no arrival time |
| `group.tryReadFrame()` | hang | async `read_frame` only |
| `abort(err: Error)` on a track or group | hang | `abort(code)` only |
| `Time.Timestamp` in a track's timescale | hang, publish | microseconds only |
| `Group.MAX_GROUP_CACHE_BYTES` and `_FRAMES` | json | not exported |

Arrival times need the Rust and JS clocks to agree on origin and units first. Tokens and
in-band AUTH are native-only too, but no consumer uses `Auth` today.
