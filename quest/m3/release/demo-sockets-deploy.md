# [S] moq.dev's hosted demos run on sockets

## Goal

moq.dev's hosted MoQ Boy and CARLA demos work with the socket versions the
cut publishes: the relays accept lite-07, the auth server and the emulator's
and worker's tokens carry the socket grants, and the site's `<moq-boy>` drops
its viewer prefix.

## Plan

Decided 2026-11-06 (maintainer, follow-up planning in `/quest-complete`): the
hosted demos ship from `release`, so they switch when the cut lands. Not
chosen: an m2 cut gate; deploying now in m0.

Main's moq-boy ([moq-dev/moq#8008](https://github.com/moq-dev/moq/pull/8008))
and CARLA ([moq-dev/moq#8011](https://github.com/moq-dev/moq/pull/8011)) talk
over sockets, which need lite-07 (or moq-transport) on every hop. Until
moq.dev's relays accept it, the released `<moq-boy>` can't connect at all.
The grants below follow [socket roles](/quest/m1/socket-roles.md); if it
hasn't landed by the cut, use the `.socket/viewer-*` and `.socket/driver-*`
grants documented in `demo/relay/prod.toml` instead.

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
  - The site's `src/components/boy.tsx` drops `prefix-viewer`.

  Then open both demos in a browser and drive a truck and a game; an agent can
  drive Chrome for that check.

Public API: none. Wire: none.
