import Button from "../shared/components/button";
import type { IconSet } from "../shared/types/icons";
import useWatchUIContext from "./useWatchUIContext";

const playbackIcons: IconSet = {
	play: () => (
		<svg
			xmlns="http://www.w3.org/2000/svg"
			width="24"
			height="24"
			viewBox="0 0 24 24"
			fill="var(--color-white)"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
			aria-hidden="true"
		>
			<path d="M5 5a2 2 0 0 1 3.008-1.728l11.997 6.998a2 2 0 0 1 .003 3.458l-12 7A2 2 0 0 1 5 19z" />
		</svg>
	),
	pause: () => (
		<svg
			xmlns="http://www.w3.org/2000/svg"
			width="24"
			height="24"
			viewBox="0 0 24 24"
			fill="var(--color-white)"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
			aria-hidden="true"
		>
			<rect x="14" y="3" width="5" height="18" rx="1" />
			<rect x="5" y="3" width="5" height="18" rx="1" />
		</svg>
	),
};

export default function PlayPauseButton() {
	const context = useWatchUIContext();
	const onClick = () => {
		context.togglePlayback();
	};

	return (
		<Button title={context.isPlaying() ? "Pause" : "Play"} onClick={onClick}>
			{context.isPlaying() ? playbackIcons.pause() : playbackIcons.play()}
		</Button>
	);
}
