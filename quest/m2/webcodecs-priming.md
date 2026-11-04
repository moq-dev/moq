# [M] js/publish corrects WebCodecs audio encoder priming

## Goal

Audio that `@moq/publish` encodes decodes in `@moq/watch` at its capture
timestamp, to within one sample, on every browser and for both Opus and AAC.

Today WebCodecs `AudioEncoder` prepends codec priming but stamps the first
chunk with the first input's timestamp, and
`js/publish/src/audio/encoder.ts` passes those stamps through. AAC priming is
1024 samples from FFmpeg-based encoders, 2112 from Apple's AudioToolbox, and
3136 in Safari, which also reads its first 1024 input samples as silence; so
browser AAC plays 21 to 65 ms late. Opus is right only when the browser emits
an OpusHead whose pre-skip the watcher trims. Found through Mediabunny's
[Project Sync](https://mediabunny.dev/blog/project-sync), which hit the same
gap ([w3c/webcodecs#626](https://github.com/w3c/webcodecs/issues/626)).

Out of scope: Rust, where `moq-audio` already declares Opus lookahead as
pre-skip and folds the AAC delay into timestamps (`folded_delay` in
`rs/moq-audio/src/encode/encoder.rs`); and playback-side sync, which
[A/V clock](/quest/m1/av-clock.md) owns.

## Plan

Decided 2026-11-04 (maintainer, /quest-plan):

- Measure the delay rather than tabulate it per browser, since it differs by
  browser, OS, and version. Once per encoder config, before the catalog lists
  it, encode an impulse through the configured `AudioEncoder`: one nonzero
  sample placed past the first 1024, so Safari's silent-input bug cannot
  swallow it. Decode it with an `AudioDecoder` configured without a
  description, so nothing trims the priming, and take the peak's offset as the
  delay. Grow the existing Opus description probe (`#probed`), which already
  runs one encode per config before listing, into this one probe for both
  codecs. Mediabunny reports the impulse correct on every browser it tried,
  for AAC and Opus.
- AAC: stamp each chunk earlier by the measured delay, so the priming lands
  before the first input sample, as `moq-audio` does. An AudioSpecificConfig
  has no field for it, so the catalog is unchanged.
- Opus: when the encoder emits no OpusHead, synthesize one carrying the
  measured pre-skip, as `moq-audio` does (`js/hang/src/util/opus.ts` reads
  them). When the emitted pre-skip disagrees with the measurement, fail the
  rendition: a wrong pre-skip cannot be repaired downstream, and failing loud
  surfaces the browser bug.
- A gap's `restart()` reconfigures the encoder, so each epoch re-primes. The
  measurement holds per config, so every epoch folds the same delay; verify
  that a restarted Chrome encoder re-primes.
- Spec: next to the Opus rule in `drafts/draft-lcurley-moq-hang.md` (the
  paragraph beginning "An Opus frame timestamp counts"), state the AAC rule: an
  AAC frame timestamp is the presentation time of its first decoded sample,
  priming included, so an encoder's priming lands before the first source
  sample. Add a changelog entry.
- Reproduce first: run the browser test below before the fix on kixtop's
  Chrome and Safari, and record the measured delays in the PR.
- Tests (decided): a standalone `test/interop/clients/js/priming.browser.ts`
  page, with no relay. It encodes an impulse at a known capture timestamp
  through `@moq/publish`'s encoder and decodes it with `@moq/watch`'s decoder.
  It asserts the decoded peak lands within one sample period of its capture
  timestamp, for each codec the browser can encode. Linux CI runs it for Opus in
  Chromium and Firefox; there, AAC skips through the existing capability probe.
  The mocked `AudioEncoder` in `js/publish/src/audio/encoder.test.ts`, given a
  stated delay, covers the AAC stamping and the Opus mismatch error. The
  [macOS browser lane](/quest/m2/macos-browser-lane.md) proves AAC in real
  browser encoders.
- Public API: none expected. An Opus catalog may now carry a `description`
  where the browser emitted none. Wire: none.

## Related

- [moq-audio in the browser](/quest/m3/audio-web.md) - its WebCodecs backend's `delay()` reuses this measurement
- [AudioToolbox encode](/quest/m2/audio-encode-audiotoolbox.md) - the native AAC encoder's delay, folded the same way
- [Chromium's Opus encoder stamps chunks from their input](/quest/m2/chromium-opus-timestamps.md) - another WebCodecs `AudioEncoder` timestamp bug
