# Migrating the C++ MoQ package

## Unreleased

### Decoder outputs take a `moq::Delay` ([#7311](https://github.com/moq-dev/moq/pull/7311))

`moq::AudioDecoderOutput` replaces `max_delay` and the playout `delay` with `std::optional<moq::Delay> delay` and `bool estimate = true`.
`moq::VideoDecoderOutput::max_delay` is `std::optional<moq::Delay> delay`.
`moq::Delay{min, max}`: `max` (`std::nullopt` for no limit) is the old `max_delay`, and `min` is the playout delay, where a cold join starts.
A fixed audio playout delay `d` is `delay = moq::Delay{d, max}` with `estimate = false`.

### Paths with an empty segment are refused ([#6648](https://github.com/moq-dev/moq/pull/6648))

Publishing, serving, or requesting a broadcast at a path with a leading, trailing, or doubled `/` fails instead of collapsing it. Pass the canonical path, such as `room/cam`.

### First release

There is no published C++ package to migrate from. Start with the current headers and examples, and build the wrapper, generated bindings, and native library together.
The underlying generated API follows the [moq-ffi migration guide](../../rs/moq-ffi/MIGRATE.md).
