# [M] HLS Playlist Tracks

## Goal

Implement and verify the behavior tracked in [#863](https://github.com/moq-dev/moq/issues/863)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

In order to get my dream of a HLS player working while pointed at `moq-relay`, we need some playlists.

Can someone modify the HLS import code such that:

1. Make sure any tracks we create have a reasonable extension, like `.m3u8` or `.m4s`
2. We create a `playlist.m3u8` track for the main playlist, with a single group/frame.
3. We create a playlist track for each rendition, for example `audio.m3u8` and `video.m3u8`.
   - Each (unique) playlist update would be written as a new group/frame.
4. Rewrite the playlists to be relative:
   - The main playlist would point to `./audio.m3u8` and `./video.m3u8` or whatever the playlist tracks are called.
   - The media playlists would point to `./audio.m4s?group=x`, or whatever the media tracks are called.

With all this in place, we should be able to start a HLS import and visit:

```
http://localhost:4443/fetch/bbb/playlist.m3u8
http://localhost:4443/fetch/bbb/audio.m3u8
http://localhost:4443/fetch/bbb/video.m3u8
http://localhost:4443/fetch/bbb/audio.m4s?group=xxx
http://localhost:4443/fetch/bbb/video.m4s?group=xxx
```

The only thing that should not work yet is the `?group=` query parameter. It will instead fetch the latest group.

With all of this in place, we could point a HLS player to the first URL and it should just work TM.

## Closes

- [#863](https://github.com/moq-dev/moq/issues/863) - close this issue when the quest finishes
