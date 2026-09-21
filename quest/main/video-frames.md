# [M] Typed pixels and one frame-conversion API

## Goal

Frame conversion keeps size and color attached to pixels, has one configured
operation per conversion, and leaves room for later packet/frame metadata.

## Plan

Collapse resize/resize_with and the RGB/BGRA configured and compatibility
spellings into one operation each. The consuming I420 path must return typed
pixels rather than dropping geometry/color into Bytes; explicit byte extraction
remains available. Use Size where both dimensions are required. Retain one
Frame/Surface hierarchy and existing native platform interoperability. Later
native imports must extend this contract additively; do not stabilize a
construction shape that the already-planned OBS bridges must replace.

Make frame and encoded-packet records extensible through constructors so later
flags, DTS, or color metadata do not require replacing them. Do not prematurely
add fields for codecs that are not implemented. Preserve backing allocation
ownership through borrowed/owned conversion and delayed consumers.

Validate dimensions and byte counts with checked arithmetic at every public
constructor/length boundary; an enormous or malformed image returns an error
instead of overflowing before validation. Tests cover extreme dimensions,
same-byte-count transposed images, metadata preservation, and borrowed versus
owned results in CI. Update all consumers and existing conversion docs.

Public API: conversion signatures/results, construction, and record
extensibility change. Wire and published C layouts: unchanged.

## Related

- [Decoded frames](/quest/next/decoded-frames.md) - later binding ownership reuses this same frame
- [Color model](/quest/next/color-model.md) - later color/HDR implementation must use the extension points
