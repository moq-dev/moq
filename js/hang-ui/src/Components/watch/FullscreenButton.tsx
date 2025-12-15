import useWatchUIContext from "./useWatchUIContext";

export default function FullscreenButton() {
	const context = useWatchUIContext();
	const onClick = () => {
		if (document.fullscreenElement) {
			document.exitFullscreen();
		} else {
			context.hangWatch.requestFullscreen();
		}
	};

	return (
		<button type="button" title="Fullscreen" class="watchControlButton" onClick={onClick}>
			⛶
		</button>
	);
}
