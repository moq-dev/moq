# Fuzzing moq-net

Coverage-guided fuzzing of the wire codecs: the moq-lite and moq-transport control
messages, stream and datagram headers, group streams through the incremental ingest,
moq-transport request-stream framing, both varint codecs, and `Path`. These are the
bytes a relay parses from an untrusted peer, so they are the surface worth fuzzing.

```bash
cargo install --locked cargo-fuzz
just rs fuzz lite
```

The targets are `lite`, `announce`, `ietf`, `varint`, `path`, `pattern`, `ingest`, and `request`. Extra arguments pass through to
libFuzzer, so `just rs fuzz lite -- -max_total_time=300` bounds a run.

The Nightly workflow runs every target for five minutes, resumes from the previous run's
minimized corpus, and uploads failure inputs as workflow artifacts.

## How it fits together

The target bodies are not in `fuzz_targets/`; they are in `moq-net`'s hidden `fuzz`
module (`src/fuzz.rs`), and the files here are one-line shims. Two reasons:

- `lite` and `ietf` are private modules, so an outside crate cannot reach a single
  decoder.
- `just check` replays the same bodies on the pinned stable toolchain, so a crash found
  here becomes a regression test that CI runs without anyone installing cargo-fuzz.

`just rs fuzz` regenerates `seeds/` from `moq_net::fuzz::seeds()` before each run, so
the corpus follows the dispatch instead of rotting beside it. libFuzzer writes what it
discovers to `corpus/<target>/`, which is kept across runs and gitignored along with
`seeds/` and `artifacts/`. `just rs fuzz --minimize <target>` shrinks
the corpus once it has grown.

Calling `cargo fuzz` directly skips `just rs fuzz`, which turns off the release
profile's fat LTO (it drops the sanitizer-coverage globals, so the link fails) and
symbol stripping. Prefix direct calls with
`CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_STRIP=none`.

## What the targets assert

Beyond "does not panic":

- A short decode requires more outer input. A field running short inside a complete
  length-prefixed body is malformed and cannot ask for bytes from the next message.
- Whatever we encode, we decode back, consuming every byte. The peer's decoder is this
  same code, so emitting something we would reject is a bug on the wire.
- Encoding is stable: encode, decode, encode again yields identical bytes. Skipped
  where a parameter map reaches the wire, since those encoders walk a `HashMap` and its
  iteration order differs per instance (moq-lite SETUP).
- A lite-07 announce stream our decoder accepts, recompressed by our encoder, resolves
  to the same announcements.
- Every path decoded from the wire (a moq-lite path, an IETF namespace or Full Track
  Name) or resolved from an announce stream parses back to itself under the strict
  constructor, and so does each run of its leading segments.
- Setup Options a client accepts from its server, a server accepts from its client: only
  PATH and AUTHORITY are one-sided.
- `Path::relative` inverts `Path::resolve`, and never produces a reference that walks
  above the root.
- A group stream ingests to the same frames and the same ending whether it arrives whole,
  a byte at a time, or in chunks the input picks, and never stalls once complete. Every
  frame matches its track's timeline, Object IDs only rise, and the frames hold no more
  bytes than the stream carried. On a clean end, where no block length is hidden, every
  byte is a frame, a header field, or a property block.
- A request-stream message is its type, a two-byte length, and exactly that body; at
  most one GOAWAY migrates it. Its body decodes as each message its type names exactly
  when the control stream's length-prefixed reading accepts it, as the same message, and
  the REQUEST\_UPDATE readers stop where the framing did.

`seeds/` also holds length prefixes at and one past each cap (a moq-lite message's size,
a moq-transport length, a frame, a property or extension block), so every rejection path
starts with an input beside it.

A refused decode is not a finding unless it asks for more bytes despite a complete
body. An encode failure is legitimate: a value can decode at a version that cannot
express it again.

## When something crashes

libFuzzer writes the input to `artifacts/<target>/crash-<hash>` and prints the `cargo fuzz`
command to reproduce it (add the prefix above). Fix the bug, then commit the input:

```bash
cp rs/moq-net/fuzz/artifacts/lite/crash-<hash> rs/moq-net/fuzz/regressions/lite/
```

`fuzz::tests::regressions` replays everything under `regressions/<target>/` as part of
the normal test suite, so the case stays covered.
