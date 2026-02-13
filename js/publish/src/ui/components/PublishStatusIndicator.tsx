import type { PublishStatus } from "../context";
import usePublishUIContext from "../hooks/use-publish-ui";

type StatusIndicatorConfig = { variant: string; text: string };

const STATUS_MAP: Record<PublishStatus, StatusIndicatorConfig> = {
	"no-url": { variant: "error", text: "No URL" },
	disconnected: { variant: "error", text: "Disconnected" },
	connecting: { variant: "connecting", text: "Connecting..." },
	"select-source": { variant: "warning", text: "Select Source" },
	"video-only": { variant: "video-only", text: "Video Only" },
	"audio-only": { variant: "audio-only", text: "Audio Only" },
	live: { variant: "live", text: "Live" },
};

const unknownStatus: StatusIndicatorConfig = { variant: "error", text: "Unknown" };

export default function PublishStatusIndicator() {
	const context = usePublishUIContext();

	const statusConfig = (): StatusIndicatorConfig => {
		const status: PublishStatus = context.publishStatus();
		return STATUS_MAP[status] || unknownStatus;
	};

	return (
		<div class="publish-ui__status-indicator flex--center">
			<span
				class={`publish-ui__status-indicator-dot publish-ui__status-indicator-dot--${statusConfig().variant}`}
			/>
			<span
				class={`publish-ui__status-indicator-text publish-ui__status-indicator-text--${statusConfig().variant}`}
			>
				{statusConfig().text}
			</span>
		</div>
	);
}
