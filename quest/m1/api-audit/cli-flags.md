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

Publishers now always serve their cleanly finished tail. The moqsink boolean
`linger` property and `<moq-publish>` attribute/control, including `BroadcastInput`
policy, are removed. NULL and `close` still interrupt immediately. The user
explicitly authorized finishing this scope on 2026-10-23, resolving the earlier
automatic approval review rejection. Default-path regressions first reproduced
the immediate unannounce/EOS behavior; mocked-time late-reader, new-epoch,
EOS-gating and interruption assertions cover the replacement behavior.

Remaining completion prerequisite:

- Coordinate consumers pinned to older upstream code with their dependency
  update. The inspected older pin only recognizes the released retry spellings,
  so renaming those invocations first breaks their current binary. Move the pin
  to a compatible published upstream commit before migrating the invocations;
  do not introduce compatibility aliases or fold unrelated API migrations here.

Public API: CLI, gst and JS breaking. Wire: none. Keep the original Goal until
the upstream landing, dependency pin and consumer migration are complete.
