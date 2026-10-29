# [XS] moq-stats refuses an oversized plain snapshot early

## Goal

moq-stats' snapshot encoder refuses an oversized plain snapshot before
parsing it back, as it already does for deflate, so the oversize tests stop
spending about 4.5 s of CPU building and re-reading a snapshot they refuse.

## Plan

Found by [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)'s
profile. Keep the refusal error the same.

Public API: none.
