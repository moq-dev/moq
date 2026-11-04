# Migrating the C++ MoQ package

## Unreleased

### Authored tracks default to a zero max age ([#7729](https://github.com/moq-dev/moq/pull/7729))

An unset `moq::TrackInfo::max_age`, and a `std::nullopt` info, now advertise a zero max age, serving only the live edge, instead of no publisher limit. Set the new `max_age_unlimited`, which ignores `max_age`, where you relied on `std::nullopt` meaning no limit. A received track's `info()` sets it when its publisher advertised no limit.

### Viewer targets are bounded ([#7090](https://github.com/moq-dev/moq/pull/7090))

Viewer ceilings and PROBE target overrides are refused with the new `moq::Error::kOutOfRange` once they would sum above 2^53 - 1 bps, the most a PROBE target carries; nothing changes on a refusal. `Viewers::watch` and `set_target` and `Viewer::update` return a `moq::expected` carrying it. Handle the new variant wherever you match the error exhaustively. Regenerate bindings and rebuild the matching native library together.

### `moq::VideoInit::lower` takes either producer kind ([#7699](https://github.com/moq-dev/moq/pull/7699))

`lower` is `std::optional<moq::MediaProducer>` instead of a `std::shared_ptr<moq::MediaTrackProducer>`. Assign `moq::MediaProducer(moq::MediaProducer::kTrack{producer})`, or `kStream` for a `moq::MediaTrackStreamProducer`.

### Media setup refusals are typed

Audio and video subscribe setup now reports `moq::Error::kUnsupportedConfig` when the decoder refuses a codec, layout, or description, or when every video decoder for the codec reports that this device cannot decode the stream. Handle this variant to try another rendition instead of inspecting an audio/video error message or `Unsupported`. Decoder faults, such as a missing driver or no device, and later peer and decode failures keep their existing errors. Regenerate bindings and rebuild the matching native library together.

### JSON writes take a broadcast-clock timestamp ([#7691](https://github.com/moq-dev/moq/pull/7691))

`moq::JsonSnapshotProducer::update`, `moq::JsonStreamProducer::append`, and `moq::JsonWindowProducer::push` and `pop` take a required `moq::Timestamp` instead of an optional one. Stamp it from `moq::MediaCatalogProducer::now()`, the broadcast clock every track shares. Their constructors refuse a track created without a `timescale`. Regenerate bindings and rebuild with the matching native library.

### Subscriptions take a `moq::Delay` ([#7697](https://github.com/moq-dev/moq/pull/7697))

`moq::Subscription::max_delay` is `moq::Delay delay`: set `delay.max` to the old `max_delay`, or start from `moq::subscription_defaults()`.
`moq::Delay::min` is a `std::optional`: `std::nullopt` leaves the target latency unstated, which a decoder plays as zero. A bare `moq::Subscription{}` leaves both bounds unset, so its `delay.max` still sets no subscriber limit.

### Announcement captures are required ([#7484](https://github.com/moq-dev/moq/pull/7484))

`moq::Announce.captures` is `std::vector<std::string>` instead of an optional vector. Remove optional checks and dereferences; an empty vector means the filter has no wildcards. Regenerate bindings and rebuild with the matching native library.

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
