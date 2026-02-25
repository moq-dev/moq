import { Button, Icon } from "@moq/ui-core";
import { createEffect, createSignal } from "solid-js";
import useWatchUIContext from "../hooks/use-watch-ui";

const getVolumeIcon = (volume: number, isMuted: boolean) => {
	if (isMuted || volume === 0) {
		return <Icon.Mute />;
	} else if (volume > 0 && volume <= 33) {
		return <Icon.VolumeLow />;
	} else if (volume > 33 && volume <= 66) {
		return <Icon.VolumeMedium />;
	} else {
		return <Icon.VolumeHigh />;
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
		<div class="watch-ui__volume-slider flex--center">
			<Button title={context.isMuted() ? "Unmute" : "Mute"} onClick={() => context.toggleMuted()}>
				{getVolumeIcon(context.currentVolume(), context.isMuted())}
			</Button>
			<input type="range" onChange={onInputChange} min="0" max="100" value={context.currentVolume()} />
			<span class="watch-ui__volume-label">{volumeLabel()}</span>
		</div>
	);
}
