# @moq/libav

[libav.js](https://github.com/Yahweasel/libav.js) built to decode Opus and AAC, for the WebCodecs polyfill [`@moq/hang`](https://www.npmjs.com/package/@moq/hang) loads when a runtime has no native `AudioDecoder`, such as Node or Bun.

libav.js publishes no variant that decodes AAC, so this is its `opus-af` variant plus ffmpeg's native AAC parser and decoder. Only the single-threaded WebAssembly target ships.

```ts
import LibAV from "@moq/libav";
import * as Polyfill from "@kixelated/libavjs-webcodecs-polyfill";

await Polyfill.load({ LibAV, polyfill: true });
```

Most apps never import this directly: `@moq/hang`'s `Util.Libav.polyfill()` does.

## Building

`dist/` is a Nix build output, not committed. The workspace's `bun install` builds it through the root `postinstall`, or rebuild it alone:

```bash
bun run --cwd js/libav build
```

Without Nix on `PATH`, as on Windows, the build unpacks the published `@moq/libav` of the same version instead, and fails if npm doesn't have that version yet.

The derivation is [`package.nix`](./package.nix): libav.js v6.10.9.0 with ffmpeg 9.0, Opus 1.6.1, and emfiberthreads 1.3, compiled with the emscripten from the flake's nixpkgs.

## License

LGPL-2.1-or-later, from ffmpeg. The corresponding source is libav.js v6.10.9.0 and the tarballs `package.nix` pins; the license texts of every component are embedded in the built JavaScript.
