<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

Media over QUIC (MoQ) is a live (media) delivery protocol utilizing QUIC.
It utilizes new browser technologies such as [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) and [WebCodecs](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API) to provide WebRTC-like functionality.
Despite the focus on media, the transport is generic and designed to scale to enormous viewership via clustered relay servers (aka a CDN).
See [moq.dev](https://moq.dev) for more information.

**Note:** this project is a [fork](https://moq.dev/blog/transfork) of the [IETF specification](https://datatracker.ietf.org/group/moq/documents/).
The principles are the same but the implementation is exponentially simpler given a narrower focus (and no politics).

# Usage

These are demos, duh.
We're using Vite but other bundlers should work too.

Run `just web` (or `bun --bun vite` from this directory) and open the pages:

- `watch.html` - Watch inspector: one tile per live broadcast discovered under a prefix, click to make a tile active (audio + a live stats panel for video/audio/network and a custom `meta.json` metadata track).
- `publish.html` - Publish from a camera/screen/file, plus an editor for the custom `meta.json` metadata track.
- `stats.html` - Relay stats dashboard: auto-discovers every node publishing `.stats` and aggregates external vs. cluster traffic. Needs `[stats] enabled = true` on the relay (the demo configs already set it).

# License

Licensed under either:

- Apache License, Version 2.0, ([LICENSE-APACHE](../../LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](../../LICENSE-MIT) or http://opensource.org/licenses/MIT)
