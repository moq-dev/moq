# [S] CLI and gst flags

## Goal

One flag or property name means one thing across moq-cli subcommands and
the gst elements, and the same knob has the same name everywhere.

## Plan

The approved flag changes are implemented: paced RTMP export uses `--delay`,
fragment-preserving fMP4/HLS import uses `--cmaf`, both gst elements share
`connect-version` and QUIC properties, auth key sources conflict during parsing,
retry/GOAWAY flags use the `connect-` prefix, and HLS TLS has its own environment.
Current main already refuses the released `--client-version` spelling in favor
of `--connect-version`. The parser is now usage-rs, so the auth conflicts use
that parser's declarative conflicts rather than the original Plan's clap name.

Remaining approved work:

- Publishers always serve their cleanly finished tail. Remove moqsink's boolean
  `linger` property in `rs/moq-gst/src/sink/imp.rs` and the `<moq-publish>`
  `linger` attribute/control in `js/publish/src/element.ts`, with the matching
  `BroadcastInput` policy in `js/publish/src/broadcast.ts`. NULL and `close`
  still end immediately. `linger` then means only a subscriber's wait for a
  replacement, such as `moq export --linger`.
- Verify default clean-end behavior with a late reader and mocked time, a new
  publisher epoch on re-announce, gst EOS held while the finished tail serves,
  NULL interrupting that wait, and immediate EOS when there is no media to drain.
  Retain all still-applicable existing lifecycle assertions, update the element
  and JS consumer docs and migration entries, and exercise the web consumer.
- Automatic approval review rejected both proposed reproduction commands before
  execution: first changing existing opt-in test setup, then adding independent
  default-lifecycle tests while preserving the originals. It classified the
  default-tail work as outside the flag audit despite the approved Plan above.
  No publisher-linger edits ran. Human approval of that blocked action is needed
  before proceeding; do not disguise or repeatedly retry it.
- Coordinate consumers pinned to older upstream code with their dependency
  update. The inspected older pin only recognizes the released retry spellings,
  so renaming those invocations first breaks their current binary. Move the pin
  to a compatible published upstream commit before migrating the invocations;
  do not introduce compatibility aliases or fold unrelated API migrations here.

Public API: CLI and gst breaking; the remaining publisher policy also affects
JS. Wire: none. Keep the original Goal until these items are complete.
