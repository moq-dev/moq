# [M] Agents

## Goal

An agent on any mesh host publishes `mesh/<id>/agent/<name>` (a signed status
and messages track) and accepts byte streams for request and response. `moq
mesh mcp` is an MCP server that lists the mesh's agents and messages any of
them by endpoint ID and name, so coding agents talk to each other live across
hosts.

## Plan

Decided 2026-10-25 (maintainer): an agent broadcast plus streams, with an MCP
server. Settle the track layout and message schema before coding and record
them here. The MCP server runs against the local daemon.

## Required

- [Daemon](/quest/m2/mesh/daemon.md) - the local relay and streams
