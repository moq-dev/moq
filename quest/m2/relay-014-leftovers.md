# [S] The relay drops its 0.14 upgrade leftovers

## Goal

moq-relay parses no setting that exists only to refuse a 0.14 config: the
ten hidden `--auth-*` flags with their environment variables and the six
hidden `[auth]` keys are gone, so each is an unknown flag or key like any
other. A public or mTLS rule may name exactly one broadcast: `anon` is the
broadcast `anon`, and the guard that refused a pattern without a wildcard is
gone from the relay and from `moq auth serve`.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Remove the hidden flags, the hidden keys, and the upgrade guard
  (rejected: keeping them another release). They guard an upgrade from 0.14
  that the `release` line (0.17) already forced, and the guard forbids a
  grant that is otherwise legal, for good.

Facts found by the audit:

- `rs/moq-relay/src/auth.rs:133-186` declares `Legacy`: `--auth-key`,
  `--auth-key-dir`, `--auth-api`, `--auth-public-api`, `--auth-domain`,
  `--auth-mtls-tier`, `--auth-tls-root`, `--auth-tls-cert`,
  `--auth-tls-key`, and `--auth-tls-disable-verify`, each hidden, each with
  a `MOQ_AUTH_*` variable. `:93-110` keeps the TOML keys `key`, `key_dir`,
  `auth_api`, `domains`, `mtls_tier`, and `tls` as `IgnoredAny`.
  `Config::deprecated` (`:199-266`) pairs each with its replacement, and
  `validate` refuses to start on any.
- All of it except the `tls` key is already on `release`
  (`origin/release:rs/moq-relay/src/auth.rs:81-135`), so an operator coming
  from 0.14 met the refusal one release ago. No quest owned the removal.
- `validate` also refuses a public pattern with no wildcard
  (`rs/moq-relay/src/auth.rs:281-301`), because 0.14 read `anon` as the
  prefix `anon/` and a pattern reads it as one broadcast.
  `moq auth serve` repeats it (`rs/moq-cli/src/auth.rs:638-664`). A
  deployment that wants to open exactly one broadcast to the public cannot
  say so.
- Tests that pin the leftovers: `released_auth_keys_refuse_to_boot`,
  the `RELEASED_AUTH_ENV` cases, and `released_auth_flag_refuses_to_boot`
  (`rs/moq-relay/src/config.rs:700-800`),
  `a_public_pattern_without_a_wildcard_refuses_to_start`
  (`rs/moq-relay/src/auth.rs:1810`), and its twin at
  `rs/moq-cli/src/auth.rs:1266`. The frozen list of released flags
  (`rs/moq-relay/tests/integration/released_cli.rs`) does not name the ten:
  they were hidden before the last release.

For the PR:

- Delete `Legacy`, the six keys, their entries in `Config::deprecated`
  (and the method, if nothing else is left in it), and their tests.
  `[auth]` is `deny_unknown_fields` like the rest of the config, so an old
  key still stops startup, with serde's message instead of a tailored one.
  Check that a `MOQ_AUTH_KEY` left in the environment is harmless: a relay
  with `auth.url` set ignores it, and a relay with no other auth setting
  already refuses to start as one that admits nobody, so neither boots
  open. The danger the refusal was written for, `key` beside `public`
  booting with every token ignored, is then a relay that serves its
  public rules and says so in its startup log.
- Delete both guards and their tests. `anon` then grants the one broadcast
  `anon`; an empty pattern names the root broadcast. Add the positive test:
  a literal public pattern admits exactly that path and nothing beneath it.
- `doc/bin/relay/auth.md:315` ("no wildcard, such as `anon`, refuses to
  start") becomes the plain rule: a pattern without a wildcard names one
  broadcast, and `anon/**` names the subtree.
- Migration note for moq-relay: the tailored refusals for 0.14 settings are
  gone, and a wildcard-free pattern is now accepted with its literal
  meaning.
- Leave the refusals for names removed since `release` (`[client]`,
  `[server.quic]`, `--server-quic-lb-*`): those protect the upgrade this cut
  asks for.

Public API: none (`Legacy` is crate-private). Config and CLI: ten hidden
flags, their variables, and six hidden keys stop parsing; a wildcard-free
public or mTLS pattern is accepted. Wire: none.

## Related

- [The relay's config names](/quest/m2/relay-config-names.md) - the old spellings this cut does refuse by name
