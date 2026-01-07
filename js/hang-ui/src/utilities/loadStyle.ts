import * as Settings from "../settings";

/**
 * Loads a CSS file from the resolved base path and injects it as a <link rel="stylesheet"> into the given shadow root or element.
 * Ensures the stylesheet is only loaded once per shadow root. Waits for basePath to be ready before resolving the asset path.
 *
 * @param {string} subpath - The subpath to the CSS file (e.g., 'styles/watch/styles.css').
 * @param {ShadowRoot|HTMLElement} shadowRoot - The shadow root or element to inject the CSS <link> into.
 * @returns {Promise<void>} Resolves when the stylesheet is loaded or already present. Logs an error if loading fails.
 */
export async function loadStyleIntoShadow(subpath: string, shadowRoot: ShadowRoot | HTMLElement): Promise<void> {
	const href = Settings.getBasePath(subpath);

	// Check if already loaded in this shadow root
	if (shadowRoot.querySelector(`link[href="${href}"]`)) {
		return;
	}

	try {
		// Create a link element
		const link = document.createElement("link");
		link.rel = "stylesheet";
		link.href = href;

		// Wait for the stylesheet to load
		await new Promise<void>((resolve, reject) => {
			link.onload = () => resolve();
			link.onerror = () => reject(new Error(`Failed to load CSS: ${href}`));
			shadowRoot.appendChild(link);
		});
	} catch (error) {
		console.error(`[hang-ui] Error loading CSS from ${href}:`, error);
	}
}
