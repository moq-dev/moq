# [S] The video decoder configures with the codec its probe accepted

## Goal

`@moq/watch`'s video support probe never mutates the catalog it is given.
When Safari rejects an `avc3.*` codec string and accepts the `avc1.*`
retry, the probe reports the codec string it accepted, and the video
decoder configures WebCodecs with that string itself. The catalog keeps
the publisher's `avc3.*`, so the catalog, the playback identity, and the
support cache all describe what the publisher sent, while the decoder
still opens on Safari. A test pins the `avc3` to `avc1` retry, which
nothing covers today.

## Plan

Found while iterating [moq-dev/moq#8261](https://github.com/moq-dev/moq/pull/8261),
which moves the probes into `js/watch/src/{audio,video}/supported.ts`. Before
it lands the probe is `supported()` in `js/watch/src/video/decoder.ts`. Today
the retry writes `config.codec = avc1` on the rendition object of the shared
catalog, so every catalog reader sees a codec the publisher never sent, and
`SupportProbe` (`js/watch/src/support-probe.ts`) recomputes its cache key
after each probe because a probe may rewrite its config. The rewrite has been
there since [moq-dev/moq#1304](https://github.com/moq-dev/moq/pull/1304) and
ships on `release`.

Decided 2026-11-08 (maintainer): ✅ the decoder picks the accepted codec; the
probe never rewrites the catalog (rejected: keep the rewrite). The probe's
result and the decoder's configure must come from one source, not two copies
of the `avc3` rule, so a custom `in.supported` that accepts a rendition
without the retry still reaches a decoder that opens on Safari.

Additive, so m1: `Decoder.supported` keeps its `(config) => Promise<boolean>`
shape, which `Source`'s `in.supported` and `Player` use unchanged, and only
stops mutating its argument.

Recommended shape: one module-private function returns the codec string
WebCodecs accepts for a config, or `undefined`; `Decoder.supported` maps it
to a boolean, and each decoder track resolves it once before its first
`configure()`, reusing it for later reconfigures. A track whose config no
codec string fits fails like any other setup error rather than configuring
with the catalog's string.

Checked for the same change elsewhere:

- `Player` passes `Video.Decoder.supported` through; it needs nothing.
- The support cache key (`decoderConfigKey`) already hashes the catalog's
  fields, which no longer change under it; drop the key recompute and its
  comment in `SupportProbe`.
- The audio probe rewrites nothing.
- No Rust or native counterpart: `moq-video` decodes from hang's parsed
  `VideoCodec`, which accepts both prefixes, and `moq-ffi` has no string
  probe.

Test, with `VideoDecoder.isConfigSupported` mocked to reject `avc3.*` and
accept `avc1.*`: an `avc3` rendition is supported, its catalog config still
reads `avc3`, and the decoder configures with `avc1`.

## Related

- [moq-dev/moq#8261](https://github.com/moq-dev/moq/pull/8261) - moves the probes into `supported.ts` and makes them the sources' default
