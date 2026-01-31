import type { Moq } from "@moq/hang";
import useWatchUIContext from "../hooks/use-watch-ui";

const MIN_RANGE = 0 as Moq.Time.Milli;
const MAX_RANGE = 5_000 as Moq.Time.Milli;
const RANGE_STEP = 100 as Moq.Time.Milli;

export default function LatencySlider() {
	const context = useWatchUIContext();
	const onInputChange = (event: Event) => {
		const target = event.currentTarget as HTMLInputElement;
		const latency = parseFloat(target.value) as Moq.Time.Milli;
		context.setJitter(latency);
	};

	return (
		<div class="latencySliderContainer">
			<label for="latency-slider" class="latencyLabel">
				Jitter:{" "}
			</label>
			<input
				id="latency-slider"
				onChange={onInputChange}
				class="latencySlider"
				type="range"
				min={MIN_RANGE}
				max={MAX_RANGE}
				step={RANGE_STEP}
				value={context.jitter()}
			/>
			<span>{typeof context.jitter() !== "undefined" ? `${Math.round(context.jitter())}ms` : ""}</span>
		</div>
	);
}
