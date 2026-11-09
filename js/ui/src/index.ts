/**
 * The base of the `@moq/watch` and `@moq/publish` browser support banners: {@link Support} and
 * the report it renders.
 *
 * Browser-only: {@link Support} extends `HTMLElement` when this module loads.
 *
 * @module
 */

/** Overlay control buttons and SVG glyphs. @internal */
export { controlButton, fullscreenButton, icon, settingsButton } from "./button";
/** Human-readable stats values. @internal */
export { formatBitrate, formatFps, formatHz, formatMillis } from "./format";
/** A rolling sparkline. @internal */
export { type Graph, type GraphOptions, graph } from "./graph";
/** Stats panel cards and key/value rows. @internal */
export { type StatCard, type StatKind, statCard, statLine } from "./stats";
export { type Level, type Report, type Show, type Status, Support } from "./support";
