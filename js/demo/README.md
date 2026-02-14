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
We're using Vite but other bundlers should just work™.

# License

Licensed under either:

-   Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
-   MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
