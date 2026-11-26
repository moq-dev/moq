# [M] Relay-free interactive demo: input upstream, video back, no relay

## Goal

A `demo/` entry where a producer hosts the MoQ session itself and a browser
viewer dials it directly, with no relay: keyboard and mouse input go upstream,
the rendered video comes back on the same session, and the page shows
input-to-photon latency (keypress to the first frame reflecting it). It runs on
any machine with a lightweight stand-in renderer, and drives a real-time AI
world model when one is configured.

## Plan

Offered in [moq-dev/moq#9571](https://github.com/moq-dev/moq/issues/9571) by
xjsXjtu2. The maintainer welcomed it there: the relay should become less and
less special.

Decided 2026-11-25 (maintainer, `/quest-import`):

- ✅ The contributor's demo lands as a new `demo/` entry (rejected: a relay-free
  mode for MoQ Boy, with the world model kept in the contributor's repo and
  linked; dropping it). This was decided knowing AGENTS.md asks before planning
  AI work here.
- ✅ The contributor builds it in their own PR, and an agent reviews it and helps
  it land (rejected: agents building it from this plan). The quest stays ready,
  so it resurfaces in triage if the PR stalls.
- ✅ The producer stack is the contributor's choice (rejected: requiring Python
  via `py/moq-rs`; requiring a Rust producer). Both can host sessions today:
  `py/moq-rs/examples/serve_clock.py` serves from Python, `moq --listen` and
  `moq-tokio` from Rust. The browser dials with a certificate fingerprint, which
  `@moq/net` supports.
- ✅ A stand-in renderer that moves on input runs behind the same producer
  interface as the model, so the demo runs without a GPU or weights and CI can
  smoke it. The world model is an opt-in backend (rejected: requiring the model,
  as CARLA requires its GPU host).

Review for: the repo's rules (`demo/` layout and justfile like the other demos,
`just check`), input as a MoQ track rather than a side channel, latency
measured on the real path, and a section in `doc/bin/demo.md`.

## Waiting

- xjsXjtu2: opens the demo's PR against `main`; check for one linked to
  [#9571](https://github.com/moq-dev/moq/issues/9571), and ask on the issue if
  none appears

## Closes

- [#9571](https://github.com/moq-dev/moq/issues/9571) - proposal to contribute a relay-free MoQ demo for real-time AI world models

## Related

- [MoQ Boy](/demo/boy/) and [CARLA](/demo/carla/README.md) - the existing demos that send input upstream, both through a relay
