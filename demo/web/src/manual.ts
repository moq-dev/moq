import "./highlight";
import "@moq/watch/ui";
import * as Catalog from "@moq/hang/catalog";
import MoqWatch from "@moq/watch/element";
import MoqWatchSupport from "@moq/watch/support/element";

export { MoqWatch, MoqWatchSupport };

const watch = document.querySelector("moq-watch") as MoqWatch | null;
if (!watch) throw new Error("missing <moq-watch> element");

const input = document.getElementById("catalog-input") as HTMLTextAreaElement;
const apply = document.getElementById("apply") as HTMLButtonElement;
const status = document.getElementById("status") as HTMLSpanElement;

const urlParams = new URLSearchParams(window.location.search);
const name = urlParams.get("broadcast") ?? urlParams.get("name");
const url = urlParams.get("url");
if (url) watch.url = url;
if (name) watch.name = name;

function setStatus(msg: string, ok = true) {
	status.textContent = msg;
	status.style.color = ok ? "" : "tomato";
}

apply.addEventListener("click", () => {
	const text = input.value.trim();
	if (!text) {
		watch.catalog = undefined;
		setStatus("cleared");
		return;
	}
	let parsed: unknown;
	try {
		parsed = JSON.parse(text);
	} catch (err) {
		setStatus(`parse error: ${(err as Error).message}`, false);
		return;
	}

	const result = Catalog.RootSchema.safeParse(parsed);
	if (!result.success) {
		setStatus(`invalid catalog: ${result.error.message}`, false);
		return;
	}

	watch.catalogFormat = "manual";
	watch.catalog = result.data;
	setStatus("applied");
});
