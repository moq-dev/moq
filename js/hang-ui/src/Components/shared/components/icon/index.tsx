import { createEffect, createSignal, type JSX } from "solid-js";
import * as Settings from "../../../../settings";

/**
 * Props for the Icon component.
 * @property name - The icon name (without the .svg extension).
 * @property class - Optional CSS class for the wrapper element.
 */
export type IconProps = {
	name: string;
	class?: string;
};

/**
 * Global cache for loaded SVG icon markup, keyed by icon name.
 * Prevents redundant network requests for the same icon.
 */
const iconCache = new Map<string, string>();

/**
 * Tracks in-flight fetch requests for icons to avoid duplicate network calls.
 */
const fetchingIcons = new Map<string, Promise<string>>();

/**
 * Icon component that loads SVG files at runtime.
 *
 * - Requires setBasePath() to be called to configure the asset location.
 * - Fetches the SVG from the configured base path and caches it globally.
 * - Always renders a <span role="img"> with the SVG as innerHTML.
 * - Sets aria-hidden to true so icons are ignored by assistive tech (decorative only).
 * - Loading and error states are exposed via data attributes for styling.
 *
 * @param props - IconProps
 * @returns JSX.Element
 */
export default function Icon(props: IconProps): JSX.Element {
	// Holds the SVG markup for the icon
	const [svg, setSvg] = createSignal<string>("");
	// True while the icon is being loaded
	const [loading, setLoading] = createSignal<boolean>(true);
	// True if the icon failed to load
	const [error, setError] = createSignal<boolean>(false);

	createEffect(() => {
		const iconName = props.name;

		// Use cached SVG if available
		const cached = iconCache.get(iconName);
		if (cached) {
			setSvg(cached);
			setLoading(false);
			setError(false);
			return;
		}

		// Use in-flight fetch if already started
		let fetchPromise = fetchingIcons.get(iconName);

		if (!fetchPromise) {
			// Start a new fetch for the icon SVG
			fetchPromise = (async () => {
				const iconPath = Settings.getBasePath(`icons/${iconName}.svg`);
				const response = await fetch(iconPath);

				if (!response.ok) {
					throw new Error(`Failed to load icon: ${iconName}`);
				}

				const svgContent: string = await response.text();
				iconCache.set(iconName, svgContent);
				return svgContent;
			})();

			fetchingIcons.set(iconName, fetchPromise);
		}

		// Update state when fetch completes
		void fetchPromise
			.then((svgContent) => {
				// Only update if this icon is still the current one
				if (props.name === iconName) {
					setSvg(svgContent);
					setLoading(false);
					setError(false);
				}
			})
			.catch((err) => {
				console.error(`Error loading icon "${iconName}":`, err);
				// Only update if this icon is still the current one
				if (props.name === iconName) {
					setLoading(false);
					setError(true);
				}
			})
			.finally(() => {
				fetchingIcons.delete(iconName);
			});
	});

	return (
		<span
			class={props.class}
			classList={{ "flex--center": true }}
			role="img"
			aria-hidden={true}
			innerHTML={svg()}
			data-icon-loading={loading() || undefined}
			data-icon-error={error() || undefined}
		/>
	);
}
