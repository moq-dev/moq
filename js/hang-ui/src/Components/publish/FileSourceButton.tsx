import { createSignal } from "solid-js";
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
			<button
				type="button"
				title="Upload File"
				onClick={onClick}
				class={`publishButton publishSourceButton ${context.fileActive() ? "active" : ""}`}
			>
				📁
			</button>
		</>
	);
}
