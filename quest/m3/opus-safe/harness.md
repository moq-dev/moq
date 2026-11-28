# [L] Compare safe Opus candidates against C libopus

## Goal

A differential harness checks any Opus implementation against C libopus, and
a written comparison of the four candidates in
[the epic](/quest/m3/opus-safe/README.md) gives the maintainer what they need
to pick one: correctness, CPU, binary size, and an estimate of the work to
finish and maintain each.

## Plan

The harness lands in this PR. It covers:

- the RFC 8251 test vectors under `opus_compare`;
- bit-exact comparison of decoded PCM on moq-audio's encoder streams (SILK,
  hybrid, CELT, stereo, surround multistream), with single, burst, and long
  losses, and DTX;
- the encoder side, comparing packets for the same PCM and settings.

#9578's Validation describes the streams it used against C 1.3.1; reuse them.

C libopus stays a test-only dependency: nixpkgs ships 1.6.1, and 1.3.1 builds
from the official tarball as #9578 did. Recommended: a decoder fuzz target
that diffs against C, beside the existing ones (`rs/moq-mux/fuzz`).

To estimate (a) and (c), convert a representative slice (such as one SILK
decode path) and measure the work it took. For (d), prototype a slice such as
the range decoder plus one CELT frame. Note that RFC 6716's normative decoder
is the C reference code itself. Measure CPU with the repo's
instruction-count benches.

When the comparison is done, add a `Maintainer:` item to
[the decoder quest](/quest/m3/opus-safe/decoder.md)'s `Waiting` that gives
the options and a recommendation, and link the comparison from it.

