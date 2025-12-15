import { For, type JSX } from "solid-js";
import useWatchUIContext from "./useWatchUIContext";

export default function QualitySelector() {
	const context = useWatchUIContext();

	const handleQualityChange: JSX.EventHandler<HTMLSelectElement, Event> = (event) => {
		const selectedValue = event.currentTarget.value || undefined;
		context.setActiveRendition(selectedValue);
	};

	return (
		<div class="qualitySelectorContainer">
			<label for="quality-select" class="qualityLabel">
				Quality:{" "}
			</label>
			<select
				id="quality-select"
				onChange={handleQualityChange}
				class="qualitySelect"
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
