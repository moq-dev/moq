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

Everything is built from this checkout: `just wasm` for the bindings, `cargo
build -p moq-relay` for the relay. `WASM_PORT` moves the relay ports (three
consecutive, from 4460), `WASM_PROFILE` picks the relay's cargo profile, and
`RELAY_BIN` points at a prebuilt relay instead.

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
`run.sh` also type-checks the harness against the generated `moq.d.ts`, which is
the only thing in the repo that reads it. That is worth doing: wasm-bindgen
resolves a type in a signature by its Rust identifier alone, so a binding can
compile, run, and still publish typings that name the wrong class.

### Relays

One per protocol flavour, since negotiation is the part that broke:

| name    | relay                            | negotiates                             |
| ------- | -------------------------------- | -------------------------------------- |
| `lite`  | defaults                         | `moq-lite-05`, over its own ALPN       |
| `ietf`  | `--listen-version moq-transport-19` | `moq-transport-19`, over its own ALPN  |
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
  which is what exercises the chunked read path in the transport adapter.
- **refuses a track the publisher does not serve** -- the refusal has to
  surface, as a rejected `subscribe` (lite-05, which looks a track's info up
  first) or as a track that yields no group (IETF, lite-02). A hang fails on the
  case timeout.

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
