# [L] Negotiate announcement takeover over IETF

## Goal

A negotiated IETF announcement can take over instances served through a live
claim and become independent of it, preserving the same readers and catalog
pins. Replacement jobs still cause Restart and never splice their content.

## Plan

Decided in the 2026-10-10 audit: replace the response-only IETF claim-epoch
proposal with the shared TAKEOVER contract. This stays m1 after the shared
model and lite-07 implementation; it does not gate the lite-07 cut.

Carry a new announcement's prefix, explicit epoch, routing metadata and
predecessor reference through the negotiated epoch extension. The predecessor
must be live on the same announcement stream; a sender without that
advertisement uses normal start, and an invalid received reference fails.
Give the new route its own ID and independent lifetime. Only matching served
instances are adopted, with the shared `Takeover` event. Response identity
alone never bypasses a requested epoch's matching-announcement requirement.

Negotiate support explicitly across the extension's drafts 17-22; an older
implementation of the epoch extension must not receive an unknown operation.
Keep epoch and cluster negotiation independent, with unambiguous framing for
epoch-only and combined sessions. No changes to unnegotiated published drafts.
A hop that cannot carry or establish takeover falls back to normal
announcement and replacement semantics. It must not invent continuity.

Run the shared takeover matrix through IETF and mixed lite-07/IETF chains,
including request aliases, joining FETCH, catalog/media pins, predecessor
Restart, filtered announcements, and legacy viewers. Reuse controlled-time
fixtures. Update the epoch extension draft and affected cluster framing in
the same PR; run `just check`, `just drafts check`, and cross-language interop.

Public API: reuse the shared operation and `Takeover` event. Wire: negotiated
IETF takeover support only; paths and unnegotiated versions stay unchanged.

## Required

- [IETF epochs](/quest/m1/ietf-epochs.md) - negotiated announcement/request identity
- [Announcement takeover](/quest/m1/announce-takeover.md) - shared semantics, model and lite-07 proof
