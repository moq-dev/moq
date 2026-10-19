# [S] Redundant ingest guide

## Goal

A setup page, `doc/setup/redundancy.md` next to Production in the Setup
sidebar, tells an operator how to make ingest redundant with multiple
routes, for any publisher rather than TS alone. Nothing says today that two
publishers announcing one path is the supported way, so operators reach for
a shared epoch instead.

It covers:

- Two publishers of one path, each minting its own epoch: the relay serves
  the newest, and losing it falls back to a still-live older one as a new
  broadcast.
- What a switch looks like: players follow the announce `Restart`,
  `moq export ts --stitch` follows as a program switch, and other exports
  end. Starting a backup is a switch too.
- Each publisher on its own path instead, so the end subscriber subscribes
  to both and decides whether to switch between, combine, or keep them
  separate (maintainer: each publisher produces its own stream, and the
  subscriber chooses).
- One publisher announced through several relays or CDNs under its own
  epoch, which resumes without a seam when a path fails.
- Where hitless importer protection lives instead: 2022-7 legs of one
  broadcast downstream, or merging legs before a single importer.

## Plan

Decided 2026-10-19 by the maintainer: a general page, not a TS section,
since multiple routes are the non-obvious part. Link it from the relay
cluster page and the `moq` publisher-runs section of `doc/bin/cli.md`.

## Required

- [Redundant importers run on distinct epochs](/quest/m0/broadcast-epoch/distinct-importer-epochs.md) - `--epoch` is removed and the failover behavior the page describes is tested

## Related

- [Identity and recovery guide](/quest/m1/identity-guide.md) - restart and takeover semantics the page links to
- [Routing guide](/quest/m1/routing-guide.md) - route ranking and federation
