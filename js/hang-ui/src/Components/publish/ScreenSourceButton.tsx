import usePublishUIContext from "./usePublishUIContext";

export default function ScreenSourceButton() {
	const context = usePublishUIContext();
	const onClick = () => {
		context.hangPublish.source.set("screen");
		context.hangPublish.invisible.set(false);
		context.hangPublish.muted.set(false);
	};

	return (
		<div class="publishSourceButtonContainer">
			<button
				type="button"
				title="Screen"
				class={`publishButton publishSourceButton ${context.screenActive() ? "active" : ""}`}
				onClick={onClick}
			>
				🖥️
			</button>
		</div>
	);
}
