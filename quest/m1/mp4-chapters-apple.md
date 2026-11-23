# [XS] Apple's players list a finished MP4's chapters

## Goal

A regular MP4 from `container::mp4` with chapters lists them in QuickTime Player
and Safari, read with AVFoundation (`AVAsset.chapterMetadataGroups(bestMatchingPreferredLanguages:)`)
rather than assumed. The chapter text track writes a `tx3g` sample entry because
mp4-atom cannot encode QuickTime's `text` entry; if Apple's reader ignores it,
carry `text` upstream in mp4-atom or find what Apple accepts.

## Plan

ffprobe lists the chapters from either form alone, which a Linux host proves.
Only a Mac reads them the way Apple's players do: write a finished file with
chapters, load it with AVFoundation, and compare titles and start times with
what `chpl` holds.

## Hosts

- macos - AVFoundation and QuickTime Player are the reader under test
