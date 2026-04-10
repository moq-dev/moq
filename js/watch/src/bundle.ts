// Entry point for the standalone CDN bundle.
//
// When loaded via a <script> tag, this registers:
//   - <moq-watch>         (from ./element)
//   - <moq-watch-ui>      (from ./ui)
//   - <moq-watch-support> (from ./support/element)
//
// NOTE: This file is only consumed by vite.config.bundle.ts when producing the
// self-contained bundle that ships under dist/bundle/. Library consumers that
// import via a bundler should continue to use "@moq/watch/element" directly.
import "./element";
import "./support/element";
import "./ui";
