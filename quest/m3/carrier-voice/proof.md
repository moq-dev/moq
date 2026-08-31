# [L] Developer-to-SIP proof

## Goal

A reproducible lab call starts when a developer client publishes a MoQ offer,
rings and answers at a real SIP endpoint, and carries bidirectional Opus audio.
A recorder or mock agent subscribes to the live call without either endpoint
creating a separate media connection.

## Plan

- Build the smallest standalone gateway beside the protocol and `moq-sip`:
  subscribe to the configured line prefix, authorize the offer, originate the
  SIP dialog, publish authoritative call state and received audio, then
  subscribe to the developer leg and send that audio as RTP. Do not wire the
  experiment into the moq.pro (downstream) edge or dashboard.
- Drive it with a Rust or TypeScript developer client and a scripted SIP
  endpoint or interoperable softphone. One command starts the relay, gateway,
  endpoint, client, and auxiliary subscriber; retained artifacts include state
  transitions, synchronized audio timestamps, and packet captures.
- Prove happy-path ringing/answer/audio and the failure paths that define the
  contract: rejection, caller cancellation, remote hangup, publisher loss,
  unauthorized line use, spoofed source identity, and an auxiliary subscriber
  without recording permission.
- Keep the second consumer passive so the proof measures MoQ's programmable
  fanout rather than designing conferencing. The gateway-to-gateway topology
  receives an architecture-level walkthrough using the same roles and wire,
  but no second gateway implementation.

## Required

- [Call fabric protocol](/quest/m3/carrier-voice/protocol.md) - fixes the wire,
  authority, and lifecycle the lab implements
- [SIP call origination](/quest/m3/carrier-voice/sip-originate.md) - supplies
  the outbound telephone leg
