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
	return "";
}

let basePath: string = detectBasePath();
let basePathPromise: Promise<string>;
let basePathResolve: ((value: string) => void) | null = null;

// Promise resolves when basePath is set (either auto or explicit)
if (basePath) {
	basePathPromise = Promise.resolve(basePath);
} else {
	basePathPromise = new Promise((resolve) => {
		basePathResolve = resolve;
	});
}

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
	if (basePathResolve) {
		basePathResolve(basePath);
		basePathResolve = null;
	}
}

/**
 * Returns a promise that resolves with the base path once it is available.
 * Used internally to ensure asset loading waits for basePath to be set.
 * @returns {Promise<string>} Promise resolving to the base path.
 */
export async function whenBasePathReady(): Promise<string> {
	if (basePath) {
		return basePath;
	}
	return basePathPromise;
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
