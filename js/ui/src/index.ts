/**
 * What the `@moq/watch` and `@moq/publish` overlays share, bundled into each: the {@link Support}
 * banner base, control buttons, stats cards, a sparkline, and formatters.
 *
 * Browser-only: {@link Support} extends `HTMLElement` when this module loads.
 *
 * @module
 */

/** Overlay control buttons and SVG glyphs. */
export { controlButton, fullscreenButton, icon, settingsButton } from "./button";
/** Human-readable stats values. */
export { formatBitrate, formatFps, formatHz, formatMillis } from "./format";
/** A rolling sparkline. */
export { type Graph, type GraphOptions, graph } from "./graph";
/** Stats panel cards and key/value rows. */
export { type StatCard, type StatKind, statCard, statLine } from "./stats";
export { type Level, type Report, type Show, type Status, Support } from "./support";
