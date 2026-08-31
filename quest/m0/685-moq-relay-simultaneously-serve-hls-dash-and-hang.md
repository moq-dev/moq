# [XL] moq-relay: Simultaneously serve HLS, DASH, and HANG

## Goal

Implement and verify the behavior tracked in [#685](https://github.com/moq-dev/moq/issues/685)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

The goal is to serve multiple protocols over the same MoQ relay, reusing the cache and allowing for gradual adoption. Any standard HLS/DASH player should work out of the box if given a correct URL. This way companies can experiment with MoQ without upgrading every single client (ex. smart TVs).

The hang publisher could create multiple tracks:

- `audio.m4s` (requires #682)
- `video.m4s`
- `init.mp4`
- `catalog.json`
- `main.m3u8` (static, lists all renditions)
- `playlist.m3u8`
- `playlist.mpd` (optional)

Each segment would be a MoQ group, and each sub-segment (aka fMP4 fragment) would be a MoQ frame. We would start with fragmenting every 250ms or something.

`catalog.json` would point to `audio.fmp4` and `video.fmp4`. The hang player would treat them like normal MoQ tracks, with the exception that I'll need to add 250ms to the jitter buffer size (add a new field to the catalog).

#### HLS Playlist

`playlist.m3u8` is where the magic happens. With #684 we could serve the playlist and segments via HTTP. We would generate a playlist that looks like:

```
#EXT-X-MAP:URI="init.mp4"
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=69&frame=0"
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=69&frame=1"
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=69&frame=2"
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=69&frame=3"
#EXTINF:1.0,
video.m4s?group=69
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=70&frame=0"
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=70&frame=1"
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=70&frame=2"
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=70&frame=3"
#EXTINF:1.0,
video.m4s?group=70
```

This will work out of the box for HLS since moq-relay concatenates frames in the same group. The `?frame` parameter would be used for LL-HLS to specify a single sub-segment instead of concatenating them.

So yeah, just point a HLS player to `https://relay.moq.dev/fetch/<broadcast>/main.m3u8`

#### HLS Playlist Updates

The only gotcha is how to actually serve this LL-HLS playlist because we want to avoid the usual F5 refresh storm that HLS is famous for.

So the idea is that the `playlist.m3u8` is split into groups/frames in such a way that we can efficiently serve it over the same (generic) moq-relay HTTP endpoint. Unfortunately, HLS players are hard-coded to send `?_HLS_msn` and `?_HLS_part` but we can reintepret them as `?group_min` and `?frame_min` respectively (to keep things generic).

The publisher could produce the playlist like this:

**group=70, frame=0:**

```
#EXT-X-MAP:URI="init.mp4"
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=69&frame=0"
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=69&frame=1"
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=69&frame=2"
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=69&frame=3"
#EXTINF:1.0,
video.m4s?group=69
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=70&frame=0"
```

**group=70, frame=1:**

```
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=70&frame=1"
```

**group=70, frame=2:**

```
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=70&frame=2"
```

**group=70, frame=3:**

```
#EXT-X-PART:DURATION=0.25,URI="video.m4s?group=70&frame=3"
#EXTINF:1.0,
video.m4s?group=70
```

When the next subsegment is generated, group 71 is created and the process repeats.

#### Conclusion

If this seems overly complicated then you're right. The goal is to make `moq-relay` generic enough that it doesn't actually understand HLS, nor does it parse the playlist. This is the type of thing that *could* be deployed as a generic MoQ CDN.

A proper media server would just parse the HLS playlist of course. That's still an option if we want to add a specialized `/hls` endpoint.

## Closes

- [#685](https://github.com/moq-dev/moq/issues/685) - close this issue when the quest finishes
