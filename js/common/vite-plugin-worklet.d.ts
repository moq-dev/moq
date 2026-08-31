import type { Plugin } from "vite";
/**
 * A Vite plugin that compiles AudioWorklet files and inlines them as blob URLs.
 *
 * Usage: import workletUrl from "./my-worklet.ts?worklet"
 *
 * The worklet file is compiled to JS with all dependencies bundled via esbuild,
 * then inlined as a string. At runtime, a blob URL is created and exported.
 * Pass the URL to audioWorklet.addModule().
 */
export declare function workletInline(alias?: Record<string, string>): Plugin;
//# sourceMappingURL=vite-plugin-worklet.d.ts.map