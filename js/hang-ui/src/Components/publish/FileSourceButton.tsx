import { createSignal } from "solid-js";
import Button from "../shared/components/button";
import Icon from "../shared/components/icon";
import usePublishUIContext from "./usePublishUIContext";

export default function FileSourceButton() {
	const [fileInputRef, setFileInputRef] = createSignal<HTMLInputElement | undefined>();
	const context = usePublishUIContext();
	const onClick = () => fileInputRef()?.click();
	const onChange = (event: Event) => {
		const castedInputEl = event.target as HTMLInputElement;
		const file = castedInputEl.files?.[0];

		if (file) {
			context.setFile(file);
			castedInputEl.value = "";
		}
	};

	return (
		<>
			<input
				ref={setFileInputRef}
				onChange={onChange}
				type="file"
				class="hidden"
				accept="video/*,audio/*,image/*"
			/>
			<Button
				title="Upload File"
				class={`publishSourceButton ${context.fileActive() ? "active" : ""}`}
				onClick={onClick}
			>
				<Icon name="file" />
			</Button>
		</>
	);
}
