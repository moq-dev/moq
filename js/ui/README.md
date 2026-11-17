# @moq/ui

Presentation code that [@moq/watch](../watch) and [@moq/publish](../publish) share: the `Support` banner base, overlay control buttons, stats cards, the sparkline, and value formatters, plus `stats.css`.

It is a private workspace package, never published. Each of the two packages bundles its code, its declarations, and its stylesheet into its own build, so their published packages import nothing from it, and anything here may change with them in any commit.
A published package lists it under `devDependencies`, and `js/common/package.ts` emits its declarations into that package's `dist/bundled/ui/`.

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](../../LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](../../LICENSE-MIT) or http://opensource.org/licenses/MIT)
