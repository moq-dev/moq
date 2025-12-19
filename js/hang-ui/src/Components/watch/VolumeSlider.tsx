import { createEffect, createSignal } from "solid-js";
import Button from "../shared/components/button";
import type { IconSet } from "../shared/types/icons";
import useWatchUIContext from "./useWatchUIContext";

const volumeIcons: IconSet = {
	mute: () => (
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
			<path d="M16 9a5 5 0 0 1 .95 2.293" />
			<path d="M19.364 5.636a9 9 0 0 1 1.889 9.96" />
			<path d="m2 2 20 20" />
			<path d="m7 7-.587.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298V11" />
			<path d="M9.828 4.172A.686.686 0 0 1 11 4.657v.686" />
		</svg>
	),
	volumeLow: () => (
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
			<path d="M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z" />
		</svg>
	),
	volumeMedium: () => (
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
			<path d="M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z" />
			<path d="M16 9a5 5 0 0 1 0 6" />
		</svg>
	),
	volumeHigh: () => (
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
			<path d="M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z" />
			<path d="M16 9a5 5 0 0 1 0 6" />
			<path d="M19.364 18.364a9 9 0 0 0 0-12.728" />
		</svg>
	),
};

const getVolumeIcon = (volume: number, isMuted: boolean) => {
	if (isMuted || volume === 0) {
		return volumeIcons.mute;
	} else if (volume > 0 && volume <= 33) {
		return volumeIcons.volumeLow;
	} else if (volume > 33 && volume <= 66) {
		return volumeIcons.volumeMedium;
	} else {
		return volumeIcons.volumeHigh;
	}
};

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
			<Button title={context.isMuted() ? "Unmute" : "Mute"} onClick={() => context.toggleMuted()}>
				{getVolumeIcon(context.currentVolume(), context.isMuted())()}
			</Button>
			<input type="range" onChange={onInputChange} min="0" max="100" value={context.currentVolume()} />
			<span class="volumeLabel">{volumeLabel()}</span>
		</div>
	);
}
