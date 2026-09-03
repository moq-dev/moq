# [M] Catalog version binding

## Goal

Every archived media group identifies the exact catalog version needed to
interpret it, including across codec or rendition changes.

## Plan

Today consumers use moq-net timestamps to select the newest catalog group that
applies to a media segment. Keep that rule for the first archive implementation.

Design an explicit version identity carried by each media group or its archive
timeline range, then update replay and HLS selection to use it. Put the binding
in the protocol model rather than an archive-only side table, since live catalog
updates have the same ambiguity.

This quest is related but does not block the initial archive.

## Required

- [Archive proof](/quest/m1/archive/proof.md) - ship and verify the timestamp-based archive first
