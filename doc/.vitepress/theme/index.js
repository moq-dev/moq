import { h } from "vue";
import DefaultTheme from "vitepress/theme";
import Banner from "./Banner.vue";
import Community from "./Community.vue";
import "./custom.css";

export default {
	extends: DefaultTheme,
	Layout() {
		// Render the site-wide notice above every page via the layout-top slot,
		// and the GitHub/Discord counts at the end of the navbar.
		return h(DefaultTheme.Layout, null, {
			"layout-top": () => h(Banner),
			"nav-bar-content-after": () => h(Community),
		});
	},
};
