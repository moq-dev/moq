# [S] Release profile: one LTO setting and a nightly size report

## Goal

The shipped moq-ffi and libmoq artifacts already build with thin LTO and one
codegen unit, but through `CARGO_PROFILE_RELEASE_*` exports in three places
(`rs/moq-ffi/build.sh`, `rs/libmoq/build.sh`, `nix/overlay.nix`), so the
Python wheel (maturin), `moq`, `moq-relay`, and every `cargo build --release`
by a self-builder get none of it, and a plain release build measures nothing a
user receives. After this quest `[profile.release]` in the workspace
`Cargo.toml` is the one place the setting lives, every release artifact gets
it, and a nightly job reports what each moq-ffi build ships, built the way
the release builds it, so size regressions are visible instead of discovered
in an app store review.

Measured on aarch64-apple-darwin, moq-ffi, stripped dylib:

| build | plain release | thin LTO, 1 CGU (as shipped) |
|---|---|---|
| default (audio + video) | 14.78 MB | 14.09 MB |
| `--no-default-features` | 13.40 MB | 12.68 MB |

The staticlib drops far more (80 MB to 30 MB for the slim build), which is
what the scripts were added for. Of the default dylib's 11.5 MiB of `.text`
before LTO: the network layer (moq_net, moq_native, noq, rustls, aws-lc,
tokio, qmux, reqwest) is about 5.4 MiB, std 1.5 MiB, moq_mux plus mp4_atom
plus hang plus the container parsers 1.2 MiB, the codecs (libopus, openh264,
symphonia, moq_audio, moq_video) 0.9 MiB, and regex 0.6 MiB, pulled by one
AV1 codec-string match in `rs/hang/src/catalog/video/av1.rs`.

## Plan

Move `lto` and `codegen-units = 1` into `[profile.release]` and delete the
three exports; the scripts' comments explaining the Go mirror's 100 MB limit
move with the setting. `profiling` and `release-with-debug` inherit it; check
both still produce symbolized captures. Thin LTO is the known-good starting
point. Try `lto = "fat"` and `strip = "symbols"` on top and keep each only if
the table earns it: thin LTO bought 5% on the dylib, so fat is not assumed to
buy much, and `strip` must not break the crash reports C ABI users read
(libmoq ships a symbols file if it does). Link time on the release matrix
goes in the same table as the sizes.

Then the nightly job: a `size` recipe under `sh/` that builds both moq-ffi
configurations and `libmoq` with the release profile, prints stripped sizes
and `cargo bloat --crates -n 30` per build, and posts the result to the job
summary. `cargo bloat` reads the symbol table, so if `strip` lands the recipe
builds with `CARGO_PROFILE_RELEASE_STRIP=none` for the bloat pass and strips
a copy for the size column. Wire it into `nightly.yml` beside `Features`. The one-line AV1 regex
in hang is worth replacing with a hand parser while the bloat table is open,
if it is what keeps regex in the link (tracing-subscriber's env filter also
uses regex-automata, so check the table rather than assume).

## Related

- [Bindgen CLI split](/quest/m2/uniffi-cli-feature.md) - the staticlib half of the same hygiene
- [Network-only bindings](/quest/m2/slim-bindings/README.md) - decides on the numbers this quest's report produces
