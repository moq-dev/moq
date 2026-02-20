import type { WatchStatus } from "../context";
import useWatchUIContext from "../hooks/use-watch-ui";

type StatusIndicatorConfig = { variant: string; text: string };

const STATUS_MAP: Record<WatchStatus, StatusIndicatorConfig> = {
	"no-url": { variant: "error", text: "No URL" },
	disconnected: { variant: "error", text: "Disconnected" },
	connecting: { variant: "connecting", text: "Connecting..." },
	offline: { variant: "error", text: "Offline" },
	loading: { variant: "loading", text: "Loading..." },
	live: { variant: "live", text: "Live" },
	connected: { variant: "connected", text: "Connected" },
};

const unknownStatus: StatusIndicatorConfig = { variant: "error", text: "Unknown" };

export default function WatchStatusIndicator() {
	const context = useWatchUIContext();

	const statusConfig = (): StatusIndicatorConfig => {
		const status: WatchStatus = context.watchStatus();
		return STATUS_MAP[status] || unknownStatus;
	};

	return (
		<div class="watch-ui__status-indicator flex--center">
			<span class={`watch-ui__status-indicator-dot watch-ui__status-indicator-dot--${statusConfig().variant}`} />
			<span class={`watch-ui__status-indicator-text watch-ui__status-indicator-text--${statusConfig().variant}`}>
				{statusConfig().text}
			</span>
		</div>
	);
}
