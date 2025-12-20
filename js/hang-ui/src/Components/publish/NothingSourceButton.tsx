import usePublishUIContext from "./usePublishUIContext";

export default function NothingSourceButton() {
	const context = usePublishUIContext();
	const onClick = () => {
		context.hangPublish.source.set(undefined);
		context.hangPublish.muted.set(true);
		context.hangPublish.invisible.set(true);
	};

	return (
		<div class="publishSourceButtonContainer">
			<button
				type="button"
				title="No Source"
				class={`publishButton publishSourceButton ${context.nothingActive() ? "active" : ""}`}
				onClick={onClick}
			>
				🚫
			</button>
		</div>
	);
}
