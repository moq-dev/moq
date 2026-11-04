# Migrating the C++ MoQ package

## Unreleased

### Publishing goes through one `moq::Route` handle

`OriginProducer::create_broadcast(path)` is `route(path, options)`, and `requests(prefix)` is `route_prefix(prefix, options)`; `{}` options are the default. Each returns a `std::shared_ptr<moq::Route>`, which replaces `moq::Publication`, `moq::OriginRequests`, and `moq::BroadcastPrepared`:

- `Publication::broadcast()` is `Route::broadcast()`, which returns an error on a prefix route.
- `Publication::announce(route)` is `announce()`, and re-pricing is `set_cost(cost)`; `unannounce()` is `withdraw()`.
- `OriginRequests::requested_broadcast()` is `requested()`, which resolves to `nullptr` once the origin closes.
- `BroadcastRequest::prepare()` then `BroadcastPrepared::publish()` is `BroadcastRequest::route(options)`: create it hidden to fill it first, then `announce()`.
- `instance()` is the stream's instance, kept across `withdraw()` and `announce()`.

`moq::AnnounceRoute` is `moq::RouteOptions{hidden, cost, instance, epoch}`, given at creation. A route is visible at once unless `hidden`; set it and call `announce()` after filling the catalog to keep the old order. Hold the route while the broadcast should stay announced: `cancel()` or releasing its last handle retracts it, and closing its broadcast ends both. `moq::Route`, the record on `moq::Announce::route`, is `moq::RouteInfo`.

### `moq::VideoInit::lower` takes either producer kind ([#7699](https://github.com/moq-dev/moq/pull/7699))

`lower` is `std::optional<moq::MediaProducer>` instead of a `std::shared_ptr<moq::MediaTrackProducer>`. Assign `moq::MediaProducer(moq::MediaProducer::kTrack{producer})`, or `kStream` for a `moq::MediaTrackStreamProducer`.

### Media setup refusals are typed

Audio and video subscribe setup now reports `moq::Error::kUnsupportedConfig` when the decoder refuses a codec, layout, or description. Handle this variant to try another rendition instead of inspecting an audio/video error message or `Unsupported`. Decoder faults, such as a backend that fails to open, and later peer and decode failures keep their existing errors. Regenerate bindings and rebuild the matching native library together.

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
