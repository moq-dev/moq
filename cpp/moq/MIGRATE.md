# Migrating the C++ MoQ package

## Unreleased

### `Following::available()` reports the path going offline ([#7678](https://github.com/moq-dev/moq/pull/7678))

`moq::Following::available()` resolves to a null `std::shared_ptr<moq::BroadcastConsumer>` once the path goes offline (or, for a follow pinned to an instance, once another instance takes it over), then the next broadcast when something serves the path again. The first call is never null. Check for null before using a later result, and treat it like the previous instance ending.

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
