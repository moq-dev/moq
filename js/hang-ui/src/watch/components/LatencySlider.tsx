import useWatchUIContext from "../hooks/use-watch-ui";

const MIN_RANGE = 0;
const MAX_RANGE = 5_000;
const RANGE_STEP = 100;

export default function LatencySlider() {
	const context = useWatchUIContext();
	const onInputChange = (event: Event) => {
		const target = event.currentTarget as HTMLInputElement;
		const latency = parseFloat(target.value);
		context.setBuffer(latency);
	};

	return (
		<div class="latencySliderContainer">
			<label for="latency-slider" class="latencyLabel">
				Buffer:{" "}
			</label>
			<input
				id="latency-slider"
				onChange={onInputChange}
				class="latencySlider"
				type="range"
				min={MIN_RANGE}
				max={MAX_RANGE}
				step={RANGE_STEP}
				value={context.buffer()}
			/>
			<span>{typeof context.buffer() !== "undefined" ? `${Math.round(context.buffer())}ms` : ""}</span>
		</div>
	);
}
