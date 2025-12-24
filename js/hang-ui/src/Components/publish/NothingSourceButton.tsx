import Button from "../shared/components/button";
import type { Icon } from "../shared/types/icons";
import usePublishUIContext from "./usePublishUIContext";

const nothingIcon: Icon = () => (
	<svg
		xmlns="http://www.w3.org/2000/svg"
		width="24"
		height="24"
		viewBox="0 0 24 24"
		fill="none"
		stroke="var(--color-red)"
		stroke-width="2"
		stroke-linecap="round"
		stroke-linejoin="round"
		aria-hidden="true"
	>
		<path d="M4.929 4.929 19.07 19.071" />
		<circle cx="12" cy="12" r="10" />
	</svg>
);

export default function NothingSourceButton() {
	const context = usePublishUIContext();
	const onClick = () => {
		context.hangPublish.source.set(undefined);
		context.hangPublish.muted.set(true);
		context.hangPublish.invisible.set(true);
	};

	return (
		<div class="publishSourceButtonContainer">
			<Button
				title="No Source"
				class={`publishSourceButton ${context.nothingActive() ? "active" : ""}`}
				onClick={onClick}
			>
				{nothingIcon()}
			</Button>
		</div>
	);
}
