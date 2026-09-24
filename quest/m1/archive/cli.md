# [M] Archive endpoint in moq-cli

## Goal

`moq-cli` records and replays archives with no custom code, through an
`archive <store-url>` endpoint on the existing direction verbs:
`export archive` records a broadcast with `moq_archive::Writer`, and
`import archive` republishes a recording, serving its groups on demand
through `moq_archive::Reader`. Recording an ingest in one process is a
multi-stage line.

```bash
moq --connect $RELAY --broadcast ev.hang export archive s3://rec/ev
moq --connect $RELAY \
    import --broadcast ev.hang srt --listen 0.0.0.0:9000 \
    -- export --broadcast ev.hang archive file:///rec/ev
moq --connect $RELAY --broadcast ev-replay.hang import archive s3://rec/ev
```

## Plan

Parse the store URL with `object_store::parse_url`. Compile in every
`object_store` cloud backend (S3, GCS, Azure) behind features, with local
files always available.

An archive stage carries exactly one broadcast; refuse a multi-broadcast
stage (an RTMP or SRT listener accepting many) rather than invent a prefix
scheme.

Export reads its own catalog: video and audio renditions enroll as pacing
tracks, the catalog and any other track as non-pacing, and renditions added
later enroll as they appear. The writer stays catalog-agnostic. Expose the
writer's retention window and deletion grace as flags.

Import hands the reader the stage's `broadcast::Producer`. The recording is
not live; decide with the reader whether import finishes the replayed
timeline or keeps following a growing archive.

Update `doc/bin/cli.md` and every example invocation.

## Required

- [Recording reader](/quest/m1/archive/reader.md) - serves `import archive`

## Closes

- [#2281](https://github.com/moq-dev/moq/issues/2281) - close this issue when the quest finishes
