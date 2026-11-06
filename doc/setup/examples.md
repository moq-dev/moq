---
title: Examples
description: Find a runnable MoQ demo or a small example to adapt in your language
---

# Examples

Choose the smallest example that exercises your feature. The links below
point to `main`; when using published packages, check the installed version
against the example's dependencies. [Upgrade](/setup/upgrade) links the
migration guides.

## Try a complete application

| Goal | Start here | Source |
| --- | --- | --- |
| Watch a stream or publish a camera | [Quick start](/setup/), then [web demo](/bin/demo#web-demo) | [`demo/web`](https://github.com/moq-dev/moq/tree/main/demo/web) |
| Build a meeting with a roster and chat | [Rooms](/lib/js/room) | [`meet.ts`](https://github.com/moq-dev/moq/blob/main/demo/web/src/meet.ts) |
| Send input back to a live application | [MoQ Boy](/bin/demo#moq-boy) | [`demo/boy`](https://github.com/moq-dev/moq/tree/main/demo/boy) |
| Stream a simulator and send controls | [CARLA](/bin/demo#carla) | [`demo/carla`](https://github.com/moq-dev/moq/tree/main/demo/carla) |
| Bring an existing media tool | [OBS](/bin/obs), [GStreamer](/bin/gstreamer), or [gateways](/bin/#gateways) | [`demo/pub`](https://github.com/moq-dev/moq/tree/main/demo/pub), [`demo/sub`](https://github.com/moq-dev/moq/tree/main/demo/sub) |

Run checkout examples inside the [Nix dev shell](/setup/dev). Each demo's
README or recipe describes its tools and inputs.

## Embed MoQ in an application

| Goal | Guide | Example source |
| --- | --- | --- |
| Add a player or publisher to a web page | [Web components](/lib/js/#web-components) | [`watch.html`](https://github.com/moq-dev/moq/blob/main/demo/web/src/watch.html), [`publish.html`](https://github.com/moq-dev/moq/blob/main/demo/web/src/publish.html) |
| Connect, discover, publish, or subscribe to raw tracks in TypeScript | [@moq/net](/lib/js/net) | [`js/net/examples`](https://github.com/moq-dev/moq/tree/main/js/net/examples) |
| Sign and verify a token in TypeScript | [@moq/auth](/lib/js/auth) | [`sign-and-verify.ts`](https://github.com/moq-dev/moq/blob/main/js/auth/examples/sign-and-verify.ts) |
| Publish or consume media in Rust | [Rust quick start](/lib/rs/#quick-start) | [`video.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/video.rs), [`subscribe.rs`](https://github.com/moq-dev/moq/blob/main/rs/hang/examples/subscribe.rs) |
| Publish a clock, serve clients, or discover broadcasts in Python | [Python](/lib/py/) | [`py/moq-rs/examples`](https://github.com/moq-dev/moq/tree/main/py/moq-rs/examples) |
| Use a native language binding | [Libraries](/lib/), then [Layers](/lib/layers) | Language guides contain samples checked by the [documentation sample harness](https://github.com/moq-dev/moq/blob/main/doc/lib/samples.sh) |

For a feature not covered by a standalone example, the
[interop clients](https://github.com/moq-dev/moq/tree/main/test/interop/clients)
exercise the same operations across languages. They are test programs with
harness-specific setup; start with the language guide before adapting them.

## Next steps

- [Inspect a relay](/bin/inspect) to see the broadcasts, catalogs, and stats produced by your example.
- [Troubleshooting](/setup/troubleshooting) routes connection and playback symptoms to the relevant guides.
- [Production](/setup/prod) covers deploying your own relay.
- [Repository map](/setup/repository) locates the implementation when you need to change MoQ itself.
