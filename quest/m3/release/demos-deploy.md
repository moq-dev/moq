# [S] moq.dev's hosted demos run the cut

## Goal

moq.dev's hosted MoQ Boy and CARLA demos work with the versions the cut
publishes. Neither uses socket endpoints: viewers and drivers publish their
input as broadcasts under the anonymous viewer prefix, so the auth server
and the tokens keep the grants that format needs, and the site builds
`<moq-boy>` from `demo/boy`.

## Plan

Decided 2026-11-06 (maintainer, follow-up planning in `/quest-complete`): the
hosted demos ship from `release`, so they switch when the cut lands. Not
chosen: an m2 cut gate; deploying now in m0.

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ the
demos go back to their format before sockets, since
[socket endpoints leave moq](/quest/m2/sockets/README.md) (rejected: the
2026-11-06 plan, which deployed socket grants and lite-07 on every hop for
them). This quest was "moq.dev's hosted demos run on sockets" until then.

What that changes here: MoQ Boy on `release` already speaks the restored
format ([its quest](/quest/m2/sockets/boy.md)), so the hosted relays need no
new version for it and the auth server keeps `demo/viewer/**`. CARLA was
added on `main` after `release` branched; its restored format
([its quest](/quest/m2/sockets/carla.md)) uses the same grant. Read both
quests' merged PRs for the grants they settled before deploying, since each
re-decides its grant rather than restoring it blindly.

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
  - The auth server's anonymous rules match `demo/relay/prod.toml` as the two
    demo quests leave it: publish under the viewer prefix (`demo/viewer/**`
    unless a demo quest narrowed it), and no `.client/*` or `.server/**`
    socket grant.
  - The emulator's token publishes its game under `demo/boy/` and subscribes
    the viewer prefix; the CARLA worker's token publishes `carla/**` and
    subscribes `viewer/carla/**`, each under the relay's `demo` root.
  - The site's `moq` submodule moves to the cut, its `moq/js/moq-boy`
    workspace becomes `moq/demo/boy`, and `src/components/boy.tsx` imports
    the element from there (`src/element.ts`), keeping `prefix-viewer`.

  Then open both demos in a browser and drive a truck and a game; an agent can
  drive Chrome for that check.

Public API: none. Wire: none.

## Required

- [MoQ Boy viewers publish their commands again](/quest/m2/sockets/boy.md) - settles the emulator's and viewers' grants this deploys
- [CARLA drivers publish their controls again](/quest/m2/sockets/carla.md) - settles the worker's and drivers' grants this deploys
