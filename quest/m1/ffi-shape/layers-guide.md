# [S] A layers guide for the bindings

## Goal

A page under `doc/lib` maps each Rust layer (net, media, json, flate, audio,
and video) to its module in every binding, and every binding page links it, so
a reader who knows one language or the Rust crates finds the same type in
another.

## Plan

Waits for [Codecs](/quest/m1/ffi-shape/codec.md), since `audio` and `video` have
no namespace to map until then (decided 2026-10-09 on #4519). Keep it a table
per layer, not a method reference: the API docs own the names (see
`doc/AGENTS.md`). C++ stays flat until
[One expected type and unprefixed names](/quest/m1/cpp-generated-shape.md)
lands, so its column names the `moq::Media*` types instead of a namespace.

## Required

- [Codecs](/quest/m1/ffi-shape/codec.md) - the `audio` and `video` namespaces the guide maps
