// Entry point for the standalone CDN bundle.
//
// When loaded via a <script> tag, this registers:
//   - <moq-publish>         (from ./element)
//   - <moq-publish-ui>      (from ./ui)
//   - <moq-publish-support> (from ./support/element)
//
// NOTE: This file is only consumed by vite.config.bundle.ts when producing the
// self-contained bundle that ships under dist/bundle/. Library consumers that
// import via a bundler should continue to use "@moq/publish/element" directly.
import "./element";
import "./support/element";
import "./ui";
