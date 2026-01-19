import Button from "../../shared/components/button/button";
import * as Icon from "../../shared/components/icon/icon";
import usePublishUIContext from "../hooks/use-publish-ui";

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
				<Icon.Ban />
			</Button>
		</div>
	);
}
