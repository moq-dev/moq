# [M] Mesh UI

## Goal

Each node publishes `mesh/<id>/.status`: its peers, links, routes, and
services, signed. A web component subscribes to it: `moq mesh ui` opens it at
a localhost page the daemon serves, showing that node's view, and a hosted
service embeds the same component to show a whole mesh through its relays.

## Plan

Decided 2026-10-25 (maintainer): the data is a track on `mesh/<id>`, not an
HTTP API; the daemon serves the page and hands it the WebTransport certificate
hash, because browsers block public pages from reaching localhost. Ship the
component as a package from `js/` so a hosted dashboard uses the same code.

## Required

- [Daemon](/quest/m2/mesh/daemon.md) - the node whose state the track carries
