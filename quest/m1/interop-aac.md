# [M] Interop subscribers check an AAC tone too

## Goal

The Rust CLI interop publisher sends an AAC tone beside its Opus one, and every
subscriber decodes and checks both renditions with `test/interop/tone.py` (the
browser with its in-page check), so AAC import and decode are proven across
clients the way Opus is.

## Plan

Split from the interop audio checks, which landed Opus only. The libav fallback
now decodes AAC, so the Node and Bun subscribers can follow.

Things to look out for:

- Every subscriber picks the first audio rendition today; checking both means
  each client decodes every rendition it is offered, or the harness asks for one
  by name per pass.
- ffmpeg's AAC encoder adds priming samples; a decoder that ignores them shifts
  the tone without a gap, which the check does not see (it does not anchor
  phase to timestamps).

Public API: none expected. Wire: none.

## Related

- [Wire-compat audio](/quest/m1/wire-compat-audio.md) - the release lanes that would follow an AAC track
