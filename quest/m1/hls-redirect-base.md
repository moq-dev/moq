# [S] HLS import resolves playlists against the redirected URL

## Goal

When an HLS import follows a same-origin redirect for a master or media
playlist, every relative URI in that playlist (child playlists, segments,
`EXT-X-MAP`, keys) resolves against the response's final URL, as RFC 8216
requires. Today a redirect from `/old/master.m3u8` to `/new/master.m3u8`
succeeds, but `video.m3u8` resolves to `/old/video.m3u8`.

## Plan

From the 2026-10-24 moq-hls audit. `Fetcher::fetch` in
`rs/moq-hls/src/import.rs` returns bytes only, so `response.url()` is
discarded; the media track keeps `self.playlist` and the master keeps
`cfg.playlist` as the base. Redirects are already restricted to the playlist's
origin ([moq-dev/moq#6374](https://github.com/moq-dev/moq/pull/6374)).

- Playlist fetches return the effective URL with the body, and each playlist
  resolves its URIs against it. The next reload of a media playlist keeps
  requesting the URL the master listed, not the redirect target, so a moving
  redirect is followed each time.
- Tests: master and media redirects to another directory resolve children
  under the new directory; a cross-origin redirect is still refused.

Public API: none. Wire: none.
