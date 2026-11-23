# [S] The cluster guide describes routing as it ships

## Goal

`doc/bin/relay/cluster.md` describes the lite-07 routing that 0.22 ships,
so an operator setting up a mesh or a local cluster reads what the relay
actually does.

## Plan

Found 2026-11-21 by the main → release routing audit:

- Lines 20-25 describe routes per prefix and a 10 s hold. The code keeps
  routes per node, with no hold (`rs/moq-net/src/route/relay.rs:27`): a claim
  with no route left is unavailable for 800 ms (25 ms × 32, `relay.rs:555-561`)
  and then lost.
- Lines 59 and 202 describe hashing the path with the node's identity. The
  code uses the draft's Rendezvous Hash of the path and the claim's Instance
  (`rs/moq-net/src/model/origin.rs:871-875`).
- Say that link costs are strictly positive (`cost = 0` has been refused
  since [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)), and
  that setting `listen.version` replaces the defaults, so enabling
  `moq-lite-07-wip` means listing every default too, since one list serves
  clients and peers alike.

[Federation](/quest/m3/cluster-routing/federation.md) adds its own recipes
later; this covers what ships at the cut.

Public API: none. Wire: none.
