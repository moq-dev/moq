import useWatchUIContext from "./useWatchUIContext";

export default function PlayPauseButton() {
	const context = useWatchUIContext();
	const onClick = () => {
		context.togglePlayback();
	};

	return (
		<button
			type="button"
			title={context.isPlaying() ? "Pause" : "Play"}
			class="watchControlButton"
			onClick={onClick}
		>
			{context.isPlaying() ? "⏸️" : "▶️"}
		</button>
	);
}
