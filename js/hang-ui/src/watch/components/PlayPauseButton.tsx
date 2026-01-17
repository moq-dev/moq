import { Show } from "solid-js";
import Button from "../../shared/components/button/button";
import * as Icon from "../../shared/components/icon/icon";
import useWatchUIContext from "../hooks/use-watch-ui";

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
