import { Show } from "solid-js";
import Button from "../shared/button";
import * as Icon from "../shared/icon";
import useWatchUIContext from "./useWatchUIContext";

export default function PlayPauseButton() {
	const context = useWatchUIContext();
	const onClick = () => {
		context.togglePlayback();
	};

	return (
		<Button title={context.isPlaying() ? "Pause" : "Play"} class="button--playback" onClick={onClick}>
			<Show when={context.isPlaying()} fallback={<Icon.Play />}>
				<Icon.Pause />
			</Show>
		</Button>
	);
}
