# [XS] One AAC codec classification for every spelling

## Goal

Every AAC codec string the player accepts is classified once, so
`mp4a.40.05` gets the same 2048-sample HE-AAC frame duration as `mp4a.40.5`,
and adding an alias cannot leave one site behind.

## Plan

Found in a 2026-10-22 media audit at
[main 1f5caa7](https://github.com/moq-dev/moq/commit/1f5caa7026bcfd8a2684da3712b384b5f74a7496)
and confirmed by source at
[main d4cdfe4](https://github.com/moq-dev/moq/commit/d4cdfe4368b17cbdf7d2a0c43d24e364a9626480).
`frameSamples` in `js/watch/src/audio/config.ts` matches only `mp4a.40.5` and
`mp4a.40.29` as HE-AAC, so `mp4a.40.05` falls through to AAC-LC's 1024
samples: 21.33 ms instead of 42.67 ms at 48 kHz. The libav fallback's set in
`js/hang/src/util/libav.ts` already accepts both spellings, which the WebCodecs
AAC registration treats as the same object type. A Legacy or LOC rendition
with no per-frame duration gets the undersized frame allowance in its playout
target.

Parse the object type out of the `mp4a.40.N` string once in `@moq/hang` and
have both sites use it, rather than a second hand-kept list. Check that Rust's
AAC codec parsing treats both spellings alike while there. Test every accepted
alias. No API or wire change.
