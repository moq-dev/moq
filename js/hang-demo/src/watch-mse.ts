import "./highlight";
import "./mse/moq-mse-player";

// Allow overriding broadcast path via query parameter
const player = document.querySelector("moq-mse-player");
if (player) {
	const urlParams = new URLSearchParams(window.location.search);
	const path = urlParams.get("path");
	if (path) {
		player.setAttribute("path", path);
	}
}
