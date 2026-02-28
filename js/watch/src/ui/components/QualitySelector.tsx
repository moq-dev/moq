import { For, type JSX } from "solid-js";
import useWatchUIContext from "../hooks/use-watch-ui";

function formatBitrate(bps: number): string {
	if (bps >= 1_000_000) {
		return `${(bps / 1_000_000).toFixed(1)} Mbps`;
	}
	if (bps >= 1_000) {
		return `${(bps / 1_000).toFixed(0)} kbps`;
	}
	return `${bps} bps`;
}

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
							{rendition.bitrate ? ` ${formatBitrate(rendition.bitrate)}` : ""}
						</option>
					)}
				</For>
			</select>
		</div>
	);
}
