import { createSignal, For, Show } from "solid-js";
import Button from "../shared/components/button";
import type { IconSet } from "../shared/types/icons";

const mediaSourceSelectorIcons: IconSet = {
	isVisible: () => (
		<svg
			xmlns="http://www.w3.org/2000/svg"
			width="12"
			height="12"
			viewBox="0 0 24 24"
			fill="var(--color-white)"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
			aria-hidden="true"
		>
			<path d="M13.73 4a2 2 0 0 0-3.46 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z" />
		</svg>
	),
	isHidden: () => (
		<svg
			xmlns="http://www.w3.org/2000/svg"
			width="12"
			height="12"
			viewBox="0 0 24 24"
			fill="var(--color-white)"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
			aria-hidden="true"
		>
			<g transform="rotate(180 12 12)">
				<path d="M13.73 4a2 2 0 0 0-3.46 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z" />
			</g>
		</svg>
	),
};

type MediaSourceSelectorProps = {
	sources?: MediaDeviceInfo[];
	selectedSource?: MediaDeviceInfo["deviceId"];
	onSelected?: (sourceId: MediaDeviceInfo["deviceId"]) => void;
};

export default function MediaSourceSelector(props: MediaSourceSelectorProps) {
	const [sourcesVisible, setSourcesVisible] = createSignal(false);

	const toggleSourcesVisible = () => setSourcesVisible((visible) => !visible);

	return (
		<>
			<Button
				onClick={toggleSourcesVisible}
				class="mediaSourceVisibilityToggle button--media-source-selector"
				title={sourcesVisible() ? "Hide Sources" : "Show Sources"}
			>
				{sourcesVisible() ? mediaSourceSelectorIcons.isVisible() : mediaSourceSelectorIcons.isHidden()}
			</Button>
			<Show when={sourcesVisible()}>
				<select
					value={props.selectedSource}
					class="mediaSourceSelector"
					onChange={(e) => props.onSelected?.(e.currentTarget.value as MediaDeviceInfo["deviceId"])}
				>
					<For each={props.sources}>
						{(source) => <option value={source.deviceId}>{source.label}</option>}
					</For>
				</select>
			</Show>
		</>
	);
}
