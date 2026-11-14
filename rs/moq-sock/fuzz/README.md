# Fuzzing moq-sock

Coverage-guided fuzzing of the receive control-message walk: the unsafe code that steps
through the kernel's control buffer (`udp::cmsg::Iter`) by the lengths written in it, and
reads each payload as a C struct (`ControlMetadata::decode` for a datagram,
`LinuxError::decode` for an error-queue entry, whose offender address an ICMP sender
chooses).

```bash
cargo install --locked cargo-fuzz
just rs fuzz sock_cmsg
```

Extra arguments pass through to libFuzzer, so `just rs fuzz sock_cmsg -- -max_total_time=300`
bounds a run. The Nightly workflow runs it for five minutes, resumes from the previous run's
minimized corpus, and uploads failure inputs as workflow artifacts. It runs only on 64-bit
Linux.

## How it fits together

The target body is not in `fuzz_targets/`; it is in `moq-sock`'s `udp::fuzz` module
(`src/udp/fuzz.rs`), because the walk is private, and so `just check` replays it on the
pinned stable toolchain and `just rs miri` runs its seeds under Miri. moq-uring's
`uring_cmsg` target reads the same input format through `udp::fuzz::Control`. See
[moq-mux's README](../../moq-mux/fuzz/README.md) for the corpus layout and calling
`cargo fuzz` directly.

## Input format

A run of control messages, then optionally a malformed end. Each message is an opcode,
then a payload length byte and the payload, zero-padded if the input runs short. An
opcode below 9 names a level and type from the table in `src/udp/fuzz.rs` (`UDP_GRO`,
`SCM_TIMESTAMPNS`, the TOS and traffic class, both packet infos, both error-queue
types); opcode 9 spells out its level and type as two native `i32`s. Opcodes 10 and 11
end the buffer with a header whose length is too short or runs past the buffer, followed
by the rest of the input; opcode 12 ends it with fewer bytes than a header. Opcodes wrap
modulo 13.

The messages are laid out as the kernel writes them: each header, its payload, and padding
to `CMSG_SPACE`.

## What the target asserts

Beyond "does not panic": the walk reports exactly what a reference model reads from the
well-formed messages' bytes, with or without the malformed end after them, so a malformed
length stops the walk and keeps everything before it. The model reads each field at its
byte offset rather than through the C structs, applies messages in order, and skips a
payload too short for its type. The reported stride is never zero, since a zero stride
would split a coalesced receive into empty datagrams forever.

## When something fails

libFuzzer writes the input to `artifacts/sock_cmsg/crash-<hash>` and prints the `cargo fuzz`
command to reproduce it. Fix the bug, then commit the input:

```bash
cp rs/moq-sock/fuzz/artifacts/sock_cmsg/crash-<hash> rs/moq-sock/fuzz/regressions/sock_cmsg/
```

`udp::fuzz::tests::regressions` replays everything under `regressions/<target>/` as part
of the normal test suite, so the case stays covered.
