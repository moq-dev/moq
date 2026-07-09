import { h } from "vue";
import DefaultTheme from "vitepress/theme";
import Banner from "./Banner.vue";
import "./custom.css";

export default {
	extends: DefaultTheme,
	Layout() {
		// Render the site-wide notice above every page via the layout-top slot.
		return h(DefaultTheme.Layout, null, {
			"layout-top": () => h(Banner),
		});
	},
};
