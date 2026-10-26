# [M] Server composition

## Goal

`moq compose` in moq-cli subscribes to a studio broadcast's sources and
layout track and publishes the composited program, headless (a GPU, or
lavapipe without one). It can push that program to RTMP and WHIP through the
studio's egress library, so a hosted service makes the fallback composite and
the creator saves the upload.

## Plan

Decided 2026-10-26 (maintainer): a moq-cli subcommand, not a mode of
`moq transcode`. A hosted service embeds the same library. It publishes its
own broadcast, never the studio's names: each publisher produces its own
stream (AGENTS.md). Update `doc/bin/cli.md`.

## Required

- [Layout track](/quest/m3/studio/compose/layout.md) - what the server composes
