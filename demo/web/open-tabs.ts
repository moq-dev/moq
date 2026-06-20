import open from "open";
import type { Plugin } from "vite";

/**
 * Dev-only plugin that opens several pages in the browser once the dev server is
 * listening. Vite's `--open` only opens one URL; this opens a tab per path so
 * `just web` brings up the watch, publish, and stats demos side by side.
 *
 * Pass `--open` to vite to also let it open the default page (don't; this
 * replaces it). Set MOQ_NO_OPEN=1 to skip opening entirely.
 */
export function openTabs(paths: string[]): Plugin {
	return {
		name: "moq-open-tabs",
		apply: "serve",
		configureServer(server) {
			if (process.env.MOQ_NO_OPEN) return;

			server.httpServer?.once("listening", () => {
				const address = server.httpServer?.address();
				const port = typeof address === "object" && address ? address.port : server.config.server.port;
				const protocol = server.config.server.https ? "https" : "http";
				const base = `${protocol}://localhost:${port}`;

				for (const path of paths) {
					open(`${base}/${path}`).catch(() => {
						// Opening a browser is best-effort (e.g. headless CI); ignore failures.
					});
				}
			});
		},
	};
}
