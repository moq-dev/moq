# [S] HLS import continues past a declared gap

## Goal

An importer skips the missing segment named by EXT-X-GAP and delivers the next playable segment, including gaps emitted by MoQ's exporter.

## Plan

A live fMP4 playlist with an init segment, a good segment, a gap URI returning 404 and more media stopped on that URI. Exercise a real playlist refresh and delivery after the gap. Keep the media sequence and discontinuity state correct. Unmarked HTTP failures retain their normal error handling.
