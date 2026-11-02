# [S] Mesh library guide

## Goal

doc.moq.dev has a mesh page for developers building on the libraries:
enrolling a node against `moq auth`, the enrollment contract a hosted
coordinator implements, `Endpoint` services and byte streams, LAN discovery
and address hints, the sparse overlay, protection levels, the `.status`
schema, and porting from iroh. It links Kixel as one binary built on them.

## Plan

Decided 2026-10-25 (maintainer): a new page, because no feature quest can
carry the whole setup. Each feature quest still updates the docs it makes
stale. Decided 2026-11-02 (maintainer, Kixel planning): installing and
running a mesh daemon is Kixel's documentation, so this page covers only the
libraries.

## Required

- [Endpoint API](/quest/m2/mesh/endpoint.md) - the API the page centers on
