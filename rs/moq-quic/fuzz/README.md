# Fuzzing moq-quic

Coverage-guided fuzzing of the QUIC state machine: the packet header and transport
parameter parsers, the frame decoder, stream bookkeeping, and a connected client and
server driven by an arbitrary op sequence. These are the bytes and events a relay
takes from an untrusted peer.

```bash
cargo install --locked cargo-fuzz
just rs fuzz connection
```

The targets are `packet`, `params`, `streamid`, `streams`, `frame`, and `connection`.
Extra arguments pass through to libFuzzer, so `just rs fuzz connection -- -max_total_time=300`
bounds a run.

The Nightly workflow runs every target for five minutes, resumes from the previous run's
minimized corpus, and uploads failure inputs as workflow artifacts.

## How it fits together

The target bodies are not in `fuzz_targets/`; they are in `moq-quic`'s hidden `fuzz`
module (`src/fuzz.rs`), and the files here are one-line shims. Two reasons:

- The frame decoder and the stream state are private, so an outside crate cannot reach
  them.
- `just check` replays the same bodies on the pinned stable toolchain, so a crash found
  here becomes a regression test that CI runs without anyone installing cargo-fuzz.

`just rs fuzz` regenerates `seeds/` from `moq_quic::fuzz::seeds()` before each run, so
the corpus follows the code instead of rotting beside it. libFuzzer writes what it
discovers to `corpus/<target>/`, which is kept across runs and gitignored along with
`seeds/` and `artifacts/`. `just rs fuzz --minimize <target>` shrinks
the corpus once it has grown.

Calling `cargo fuzz` directly skips `just rs fuzz`, which turns off the release
profile's fat LTO (it drops the sanitizer-coverage globals, so the link fails) and
symbol stripping. Prefix direct calls with
`CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_STRIP=none`.

## What the targets assert

`packet`, `params`, `streamid`, and `streams` are ported from upstream quinn. Beyond
"does not panic":

- A decoded packet header and its coalesced remainder account for every input byte.
- Transport parameters that decode re-encode to bytes that decode to the same values.
- A stream ID keeps the initiator, direction, and index it was built from.
- ACK ranges walk downward without overlapping.
- `connection` handshakes a client and server, then applies opens, accepts,
  writes, finishes, resets (plain and at an offset), acknowledgment queries, stops, reads that hold or
  release credit, leases that wait for a unit, eviction ranks, runtime limit and window changes, datagrams, key updates, and time
  jumps, while dropping, duplicating, reordering, corrupting, and injecting datagrams.
  Every stream read is in order, never past what was written, byte-for-byte what was
  written, and finishes only at the length the writer finished at. Every datagram
  carries a sequence number and arrives intact, at most once. After every op, the
  credit each side's receive leases hold sums to what the connection counts as held,
  and every leased stream holding credit is ranked for eviction. The network drains at the
  end, so whatever is still in flight is checked too.

  Both sides run `fuzz::Plaintext`, crypto with no TLS and no packet protection, so a
  run skips the TLS handshake and a corrupted or injected packet decrypts and reaches
  the frame layer. Since such a packet can carry valid STREAM and DATAGRAM frames, the
  content checks skip the streams and datagrams it carried; the read order check and
  "does not panic" still cover them. `Plaintext` is only reachable through the `fuzz`
  feature, which the benches also enable, so it never ships in a normal build.

A parse that fails is not a finding, and neither is a connection the ops close.

## When something crashes

libFuzzer writes the input to `artifacts/<target>/crash-<hash>` and prints the `cargo fuzz`
command to reproduce it (add the prefix above). `cargo fuzz tmin` shrinks it. Fix the bug,
then commit the input:

```bash
cp rs/moq-quic/fuzz/artifacts/connection/crash-<hash> rs/moq-quic/fuzz/regressions/connection/
```

`fuzz::tests::regressions` replays everything under `regressions/<target>/` as part of
the normal test suite, so the case stays covered. A crash whose fix belongs in its own
change lands first as an `#[ignore]`d test that replays it and names that change.
