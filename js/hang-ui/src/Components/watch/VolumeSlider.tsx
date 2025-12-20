import { createEffect, createSignal } from "solid-js";
import useWatchUIContext from "./useWatchUIContext";

export default function VolumeSlider() {
	const [volumeLabel, setVolumeLabel] = createSignal<number>(0);
	const context = useWatchUIContext();

	const onInputChange = (event: Event) => {
		const el = event.currentTarget as HTMLInputElement;
		const volume = parseFloat(el.value);
		context.setVolume(volume);
	};

	createEffect(() => {
		const currentVolume = context.currentVolume() || 0;
		setVolumeLabel(Math.round(currentVolume));
	});

	return (
		<div class="volumeSliderContainer">
			<button
				type="button"
				title={context.isMuted() ? "Unmute" : "Mute"}
				class="watchControlButton"
				onClick={() => context.toggleMuted()}
			>
				{context.isMuted() ? "🔇" : "🔊"}
			</button>
			<input type="range" onChange={onInputChange} min="0" max="100" value={context.currentVolume()} />
			<span class="volumeLabel">{volumeLabel()}</span>
		</div>
	);
}
