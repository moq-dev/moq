import { For, Show } from "solid-js/web";
import { Time } from "@moq/lite";

type BufferRange = {
	start: Time.Micro | undefined;
	end: Time.Micro | undefined;
};

type BufferTimelineProps = {
	ranges: BufferRange[];
	currentTime: Time.Micro | undefined;
	latency: number; // in milliseconds - used as window size
	isPlaying?: boolean; // defaults to true
};

const MIN_WIDTH_PERCENT = 1;
const MS_PRECISION = 1;
// use a fixed window size since a dynamic window size could cause
// the buffer representations to jump around a lot
const MAX_WINDOW_SIZE = 5_000_000;

export default function BufferTimeline(props: BufferTimelineProps) {
	const timelineData = () => {
		const ranges = props.ranges.filter(
			function removeEmptyRanges(r): r is { start: Time.Micro; end: Time.Micro } {
				return r.start !== undefined && r.end !== undefined;
			}
		);

		const currentTime = props.currentTime ?? 0;

		if (ranges.length === 0) {
			return { ranges: [], currentTime, windowSize: 0, behindLive: 0 };
		}

		// "Behind live" = latency setting (how far playing lags behind live)
		const behindLive = props.latency * 1000;

		// Window size = max of latency and actual buffer extent (so all buffers fit)
		const windowSize = MAX_WINDOW_SIZE;

		return { ranges, currentTime, windowSize, behindLive };
	};

	const getRangeStyle = (range: { start: Time.Micro; end: Time.Micro }) => {
		const { currentTime, windowSize } = timelineData();
		if (windowSize === 0) return { left: "0%", width: "0%", visible: false };

		// Calculate position relative to current time
		const offsetStart = range.start - currentTime;
		const offsetEnd = range.end - currentTime;

		// Skip ranges that are entirely in the past or has no duration
		if (offsetEnd <= 0) {
			return { left: "0%", width: "0%", visible: false };
		}

		// Clamp start to 0 (current time)
		const clampedStart = Math.max(offsetStart, 0);
		const duration = Math.min(offsetEnd - clampedStart, windowSize - offsetStart);

		const leftPercent = (clampedStart / windowSize) * 100;
		const widthPercent = (duration / windowSize) * 100;
		
		const finalWidth = Math.max(MIN_WIDTH_PERCENT, widthPercent);

		return {
			left: `${leftPercent}%`,
			width: `${finalWidth}%`,
			visible: true,
		};
	};

	const formatMs = (micro: Time.Micro) => {
		const ms = Time.Micro.toMilli(micro);
		if (ms < 1000) {
			return `${ms.toFixed(MS_PRECISION)} ms`;
		}
		return `${(ms / 1000).toFixed(MS_PRECISION)} s`;
	};

	const formatBehindLive = () => {
		const seconds = props.latency / 1000;
		return `${seconds.toFixed(MS_PRECISION)} s behind live`;
	};

	const formatWindowSize = () => {
		return formatMs(MAX_WINDOW_SIZE as Time.Micro);
	};

	const getStartOffset = (range: { start: Time.Micro; end: Time.Micro }) => {
		const { currentTime } = timelineData();
		const offset = Math.max(range.start - currentTime, 0) as Time.Micro;
		return `+${formatMs(offset)} ahead`;
	};

	const getDuration = (range: { start: Time.Micro; end: Time.Micro }) => {
		const duration = (range.end - range.start) as Time.Micro;
		return `${formatMs(duration)} duration`;
	};

	return (
		<div class="bufferTimeline">
			<div class="bufferTimelinePlayheadLabel">
				{props.isPlaying !== false ? (
					<>Playing <span class="bufferTimelineBehindLive">({formatBehindLive()})</span></>
				) : (
					"Not Playing"
				)}
			</div>
			<div class="bufferTimelineTrack">
				<span class="bufferTimelineWindowLabel">{formatWindowSize()}</span>
				<For each={timelineData().ranges}>
					{(range) => {
						const style = getRangeStyle(range);
						if (!style.visible) return null;
						return (
							<div
								class="bufferTimelineRange"
								style={{ left: style.left, width: style.width }}
							>
								<span class="bufferTimelineStartLabel">
									{getStartOffset(range)}
								</span>
								<span class="bufferTimelineEndLabel">
									{getDuration(range)}
								</span>
							</div>
						);
					}}
				</For>
			</div>
		</div>
	);
}
