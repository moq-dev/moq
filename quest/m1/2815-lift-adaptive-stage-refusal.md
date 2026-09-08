# [XS] moq-cli runs several adaptive import stages on one connection

## Goal

Two encoding stages over one connection, such as `moq ... capture -- capture`,
are accepted, and both encoders target shares of the connection's estimate that
sum to at most it, surplus left unclaimed when a ceiling binds. The refusal in `rs/moq-cli/src/args.rs`, "a stage that encodes to fit
the connection's bandwidth estimate assumes it's the only publisher", is gone
with the test that asserts it.

## Plan

The allocator #2854 landed on dev is what the refusal stood in for: `main.rs`
mints one `bandwidth::Allocator` per connection and every encoding sender
reserves against it (the `moq-video` encode producer, `moq-audio` capture), so
the guard refuses a configuration the allocator already divides by track
priority.

- Delete the `adaptive` / `imports == 1` ensure and
  `an_adaptive_capture_must_be_the_only_import`. Keep
  `audio_only_capture_is_not_bandwidth_adaptive` only if `uses_bandwidth` still
  has a reader; otherwise delete both.
- Regression: two capture stages on one connection whose grants sum to at most
  the estimate and rank by priority, next to the allocator's
  `concurrent_tracks_split_the_estimate`, plus an args test that runs the same
  validation entry point the CLI does and accepts the combination, not one that
  only parses it.
- Audio reserves its configured rate and does not follow a smaller grant until
  [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md);
  that is the allocator's documented advisory contract and holds for one
  capture stage exactly as for two, so it is not a reason to keep the refusal.
- `doc/bin/cli.md` "Multiple stages" drops any mention of the limit.

Branch from dev.

## Closes

- [#2815](https://github.com/moq-dev/moq/issues/2815) - close this issue when the quest finishes

## Related

- [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - audio following its grant
- [#2859](/quest/m1/2859-passthrough-imports-reserve-no-bandwidth-so-a-co-resident.md) - passthrough imports joining the same allocator
