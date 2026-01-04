import Button from "../shared/components/button";
import Icon from "../shared/components/icon";
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
			<Button
				title="No Source"
				class={`publishSourceButton ${context.nothingActive() ? "active" : ""}`}
				onClick={onClick}
			>
				<Icon name="ban" />
			</Button>
		</div>
	);
}
