# [S] moq.dev's hosted demos run on sockets

## Goal

moq.dev's hosted MoQ Boy and CARLA demos work with the socket versions the
cut publishes: the relays accept lite-07, the auth server and the emulator's
and worker's tokens carry the socket grants, and the site builds `<moq-boy>`
from `demo/boy` without a viewer prefix.

## Plan

Decided 2026-11-06 (maintainer, follow-up planning in `/quest-complete`): the
hosted demos ship from `release`, so they switch when the cut lands. Not
chosen: an m2 cut gate; deploying now in m0.

Main's moq-boy ([moq-dev/moq#8008](https://github.com/moq-dev/moq/pull/8008))
and CARLA ([moq-dev/moq#8011](https://github.com/moq-dev/moq/pull/8011)) talk
over sockets, which need lite-07 (or moq-transport) on every hop, so the
sockets `<moq-boy>` can't connect until moq.dev's relays accept it.
The grants below are the server and client socket roles that
`demo/relay/prod.toml` documents.

The cut publishes no MoQ Boy package
([moq-dev/moq#8446](https://github.com/moq-dev/moq/pull/8446), 2026-11-10):
the `moq-boy` crate is `publish = false` and the element lives in the private
`demo/boy` (`@moq/demo-boy`), which has no `exports`. The site builds the
element from its `moq` submodule, and on 2026-11-12 it still lists the
`moq/js/moq-boy` workspace and imports `@moq/boy/element`, both gone at the
cut. The demo hosts keep building the emulator from the flake's `moq-boy`
package.

## Waiting

- Maintainer: after the cut publishes, deploy moq.dev:
  - Relays accept lite-07 in `listen.version` (the opt-in
    `moq-lite-07-wip` until the cut finalizes it).
  - The auth server's anonymous rules match `demo/relay/prod.toml`: publish
    `demo/boy/.client/*` and `demo/carla/.client/*`, subscribe their
    `.server/**`, and no `demo/viewer/**`.
  - The emulator's token publishes `demo/boy/.server/<game>` and subscribes
    `demo/boy/.client/**`; the CARLA worker's token publishes its trucks'
    `.server/<truck>` endpoints and subscribes `demo/carla/.client/**`.
  - The site's `moq` submodule moves to the cut, its `moq/js/moq-boy`
    workspace becomes `moq/demo/boy`, and `src/components/boy.tsx` imports the
    element from there (`src/element.tsx`) and drops `prefix-viewer`, which
    the element no longer reads.

  Then open both demos in a browser and drive a truck and a game; an agent can
  drive Chrome for that check.

Public API: none. Wire: none.
