# [M] Processor media contract

## Goal

The Hang catalog defines an external processor contribution reference, output
source relation, and correlation contract that Rust and JavaScript consumers
can resolve without eagerly subscribing to processor output.

## Plan

Build on hang's existing per-rendition relative `broadcast` reference
(`broadcast` on the video rendition in `rs/hang/src/catalog/video/mod.rs`,
with its audio, text, JSON, and binary counterparts): it already names another
broadcast relative to the catalog's, and Rust rejects one that escapes. The
processor output lives under the processor's prefix, mirroring the source
path, so the source catalog reaches it through that relative reference. Add
only what it lacks: a catalog-level contribution entry that names a whole
contribution without opening it, which the processor contract specializes.

The processor publishes a normal Hang contribution rather than an arbitrary
fragment for an edge to merge. Output renditions carry their own schema and an
explicit source relation.

Frame-derived output identifies the source video rendition, group sequence, and
frame ordinal. Audio and text processors use the source timing contract for
their rendition while retaining their own group identity. Version the relation
for future cross-broadcast inputs; the first version relates only to tracks
carried by the named source broadcast.

Land Rust and JavaScript catalog bindings, resolver behavior, and fixtures for
video, audio, text, missing output, malformed relations, lazy resolution, and
relative-path escape. The release and the moq.pro (downstream) pin rollout
stay out of this quest.

## Required

- [Catalog track alias](/quest/m1/catalog-track-alias.md) - renditions from a source and an output broadcast that share a track name would otherwise collide in one catalog
