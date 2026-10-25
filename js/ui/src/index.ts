/**
 * Presentation components shared by the `@moq/watch` and `@moq/publish` overlays: control
 * buttons, a sparkline graph, and the base of their browser support banners.
 *
 * Browser-only: {@link Support} extends `HTMLElement` when this module loads.
 *
 * @module
 */

export { controlButton, icon, settingsButton } from "./button";
export { type Graph, type GraphOptions, graph } from "./graph";
export { type Level, type Report, type Show, type Status, Support } from "./support";
