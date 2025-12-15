import { Show } from "solid-js";
import useWatchUIContext from "./useWatchUIContext";

export default function BufferingIndicator() {
	const context = useWatchUIContext();

	return (
		<Show when={context.buffering()}>
			<div class="bufferingContainer">
				<div class="bufferingSpinner" />
			</div>
		</Show>
	);
}
