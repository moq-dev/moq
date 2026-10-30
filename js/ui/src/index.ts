/**
 * Presentation components shared by the `@moq/watch` and `@moq/publish` overlays: control
 * buttons, a fullscreen controller, stats cards and formatters, a sparkline graph, and the base
 * of their browser support banners.
 *
 * Browser-only: {@link Support} extends `HTMLElement` when this module loads. The stats card and
 * graph styles ship as `@moq/ui/stats.css`, for an overlay to `@import` into its stylesheet.
 *
 * @module
 */

export { controlButton, fullscreenButton, icon, settingsButton } from "./button";
export { formatBitrate, formatFps, formatHz, formatMillis } from "./format";
export { createFullscreen, type Fullscreen } from "./fullscreen";
export { type Graph, type GraphOptions, graph } from "./graph";
export { type StatCard, type StatKind, statCard, statLine } from "./stats";
export { type Level, type Report, type Show, type Status, Support } from "./support";
