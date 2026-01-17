import { createSignal, For, Show } from "solid-js";
import Button from "../../shared/components/button/button";
import * as Icon from "../../shared/components/icon/icon";

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
				<Show when={sourcesVisible()} fallback={<Icon.ArrowDown />}>
					<Icon.ArrowUp />
				</Show>
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
