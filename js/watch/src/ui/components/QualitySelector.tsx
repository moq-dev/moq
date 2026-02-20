import { For, type JSX } from "solid-js";
import useWatchUIContext from "../hooks/use-watch-ui";

export default function QualitySelector() {
	const context = useWatchUIContext();

	const handleQualityChange: JSX.EventHandler<HTMLSelectElement, Event> = (event) => {
		const selectedValue = event.currentTarget.value || undefined;
		context.setActiveRendition(selectedValue);
	};

	return (
		<div class="watch-ui__quality-selector">
			<label for="quality-select" class="watch-ui__quality-label">
				Quality:{" "}
			</label>
			<select
				id="quality-select"
				onChange={handleQualityChange}
				class="watch-ui__quality-select"
				value={context.activeRendition() ?? ""}
			>
				<option value="">Auto</option>
				<For each={context.availableRenditions() ?? []}>
					{(rendition) => (
						<option value={rendition.name}>
							{rendition.name}
							{rendition.width && rendition.height ? ` (${rendition.width}x${rendition.height})` : ""}
						</option>
					)}
				</For>
			</select>
		</div>
	);
}
