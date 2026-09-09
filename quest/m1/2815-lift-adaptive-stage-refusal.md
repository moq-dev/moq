# [XS] moq-cli runs several adaptive import stages on one connection

## Goal

Two encoding stages over one connection, `moq --connect <url> import capture
-- import capture`, are accepted, and both encoders target shares of the
connection's estimate that sum to at most it, surplus left unclaimed when a
ceiling binds. The refusal in `rs/moq-cli/src/args.rs:277-282`, "a stage that
encodes to fit the connection's bandwidth estimate assumes it's the only
publisher", is gone with the test that asserts it.

## Plan

The allocator #2854 landed on dev is what the refusal stood in for:
`spawn_moq` mints one `bandwidth::Allocator` per connection
(`rs/moq-cli/src/main.rs:295-319`) and clones it into every import stage
(`main.rs:367`, `:380`), and every encoding sender reserves against it: the
moq-video capture loop (`rs/moq-video/src/encode/producer.rs:467-469`) and
moq-audio's `publish_capture` driver
(`rs/moq-audio/src/encode/capture.rs:471-474`). So the guard refuses a
configuration the allocator already divides by track priority
(`rs/hang/src/catalog/priority.rs:21-26`).

- Delete the `adaptive` / `imports == 1` ensure and
  `an_adaptive_capture_must_be_the_only_import` (`args.rs:1175`). Keep
  `audio_only_capture_is_not_bandwidth_adaptive` (`:1233`) only if
  `uses_bandwidth` (`:690`) still has a reader; otherwise delete both.
- Acceptance stays at the args and allocator unit level, since two
  default-device captures would open the same camera: an args test that runs
  `Invocation::validate`, the entry point the CLI uses, and accepts the
  combination rather than only parsing it; and a test next to the allocator's
  `concurrent_tracks_split_the_estimate` in
  `rs/moq-net/src/model/bandwidth.rs` where two video wants on one estimate
  get grants summing to at most it and ranked by priority.
- Audio reserves its configured rate and does not follow a smaller grant until
  [#2848](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md);
  that is the allocator's documented advisory contract and holds for one
  capture stage exactly as for two, so it is not a reason to keep the refusal.

Branch from dev.

## Closes

- [#2815](https://github.com/moq-dev/moq/issues/2815) - close this issue when the quest finishes

## Related

- [#2848](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - audio following its grant
- [#2859](/quest/m1/2859-passthrough-imports-reserve-no-bandwidth-so-a-co-resident.md) - passthrough imports joining the same allocator
