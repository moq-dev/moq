# [XS] A hung Python test fails instead of hanging the suite

## Goal

Every test that `just py test` runs (`py/moq-rs/tests/` and
`py/moq-ffi/tests/`) has a time bound, so a regression that leaves a read or
loop waiting forever fails that test with a traceback rather than hanging
`just py test` and the gate. Done when a test that awaits forever fails
within the bound, and the suite passes as before.

## Plan

Planned 2026-11-04 (maintainer, `/quest-plan`), from the follow-ups of
[moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729): Python tests
that loop over groups without a timeout hang instead of failing.

On `origin/main` (`dc0bd09b11`), 33 test functions and helpers loop or
await a read with no `wait_for` or `timeout` anywhere in their bodies. A few
of the helpers may be bounded by their callers. Most are in
`py/moq-rs/tests/test_local.py`, for example `test_local_publish_consume_audio`,
`test_raw_publish_consume`, `test_raw_group_sequence`, and the `routes` and
`until_wanted` helpers. `py/moq-rs/tests/test_server.py` has more, for
example `test_server_client_roundtrip` and `test_broadcast_route_over_wire`,
plus its own `routes`. Other tests already wrap their reads in
`asyncio.wait_for` or `asyncio.timeout` one at a time (89 calls), so the suite has no single
rule.

Recommended, as the simplest approach that fits: add `pytest-timeout` to the
root `pyproject.toml`'s `dev` dependency group and set one `timeout` in the
existing `[tool.pytest.ini_options]`. Size it well above the slowest test's
loaded time, measured with `pytest --durations`. One setting covers every
current and future test, including sync ones, while per-loop deadlines would
mean editing about 30 sites and remembering the rule in every new test.

- Keep the existing `wait_for` calls. They give a precise failure, and the
  global bound only catches what they miss.
- Check which `timeout_method` fires on a hang inside a native moq-ffi call
  that holds the GIL. The default `signal` method can't interrupt that, and
  `thread` can, at the cost of ending the run. Pick the method that fires
  for both an awaited hang and a native one, and note why in the config.
- Update `uv.lock`. Dependabot's `uv` entry for `/` then covers the new
  dependency.

Public API: none. Wire: none.
