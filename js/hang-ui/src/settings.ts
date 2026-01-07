/**
 * Attempts to auto-detect the base path for assets by inspecting the script tag that loaded hang-ui.
 * Returns the directory containing the script, or an empty string if not found.
 * @returns {string} The detected base path or an empty string.
 */
function detectBasePath(): string {
	const script = document.querySelector<HTMLScriptElement>('script[src*="hang-ui"]');
	if (script?.src) {
		const url = new URL(script.src);
		url.pathname = url.pathname.substring(0, url.pathname.lastIndexOf("/"));
		return url.origin + url.pathname;
	}
	return "/@moq/hang-ui";
}

let basePath: string = detectBasePath();

/**
 * Sets the base path for loading assets (icons, CSS, etc.).
 * This overrides the auto-detected path. Should be called before any asset loading.
 *
 * @param {string} path - The base path to use (should not end with a slash).
 * @example
 * setBasePath('/node_modules/@moq/hang-ui/dist');
 * setBasePath('https://cdn.example.com/hang-ui/v0.1.0');
 * setBasePath('/assets/hang-ui');
 */
export function setBasePath(path: string): void {
	basePath = path.replace(/\/$/, "");
}

/**
 * Constructs a full asset URL by joining the base path and the provided subpath.
 * If no subpath is given, returns the base path itself.
 *
 * @param {string} [subpath] - Optional subpath to append to the base path.
 * @returns {string} The full asset URL.
 */
export function getBasePath(subpath = ""): string {
	if (!subpath) {
		return basePath;
	}
	return `${basePath}/${subpath.replace(/^\//, "")}`;
}
