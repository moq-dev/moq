import { Assets } from "@moq/hang/util";

/** This package's hosted directory, set by {@link assets}. */
export const hosted = new Assets.Directory();

/**
 * Load the audio worklet and the libav polyfill from files under `url` instead of blob: URLs and bundler paths, for pages whose CSP refuses blob:.
 *
 * Copy `@moq/watch/assets/*` into that directory, again on every upgrade. Applies to media started afterwards.
 */
export function assets(url: string | URL): void {
	hosted.set(url);
}
