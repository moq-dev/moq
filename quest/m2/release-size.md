# [S] Release profile: link-time optimization and a nightly size report

## Goal

`[profile.release]` sets only `panic = "abort"`: no `lto`, sixteen codegen
units, symbols kept. `wasm-release` already uses `lto = "fat"`,
`codegen-units = 1`, and `strip = true`. Every shipped native artifact (the
moq-ffi libraries under five language packages, `libmoq.a`, `moq`, and
`moq-relay`) pays for that gap. After this quest the release profile uses
link-time optimization, and a nightly job reports what each moq-ffi build
ships so size regressions are visible instead of discovered in an app store
review.

Baseline, release on aarch64-apple-darwin:

| moq-ffi build | dylib stripped | staticlib | crates |
|---|---|---|---|
| default (audio + video) | 14.78 MB | 95 MB | 349 |
| `--no-default-features` | 13.40 MB | 80 MB | 303 |

Of the default dylib's 11.5 MiB of `.text`: the network layer (moq_net,
moq_native, noq, rustls, aws-lc, tokio, qmux, reqwest) is about 5.4 MiB, std
1.5 MiB, moq_mux plus mp4_atom plus hang plus the container parsers 1.2 MiB,
the codecs (libopus, openh264, symphonia, moq_audio, moq_video) 0.9 MiB, and
regex 0.6 MiB, pulled by one AV1 codec-string match in
`rs/hang/src/catalog/video/av1.rs`.

## Plan

Set `lto = "fat"`, `codegen-units = 1`, and `strip = "symbols"` on
`[profile.release]`, matching `wasm-release`. `profiling` and
`release-with-debug` inherit it and re-enable what they need; check both still
produce symbolized captures. If a fat-LTO link is too slow for the release
matrix, `lto = "thin"` is the fallback and its size difference goes in the PR.
`strip` must not break the crash reports the C ABI users read; libmoq
consumers get their own symbols file if it does.

Measure before and after on the same host: stripped dylib size for the two
moq-ffi builds above, `libmoq.a`, and the `moq` and `moq-relay` binaries,
plus wall-clock link time. The table goes in the PR description.

Then the nightly job: a `size` recipe under `sh/` that builds both moq-ffi
configurations and `libmoq`, prints stripped sizes and
`cargo bloat --crates -n 30` per build, and posts the result to the job
summary. Wire it into `nightly.yml` beside `Features`. The one-line AV1 regex
in hang is worth replacing with a hand parser while the bloat table is open,
if it is what keeps regex in the link (tracing-subscriber's env filter also
uses regex-automata, so check the table rather than assume).

## Related

- [Bindgen CLI split](/quest/m2/uniffi-cli-feature.md) - the staticlib half of the same hygiene
- [Network-only bindings](/quest/m2/slim-bindings/README.md) - decides on the post-LTO numbers this quest produces
