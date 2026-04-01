export default {
	plugins: [
		"remark-frontmatter",
		"remark-preset-lint-consistent",
		"remark-preset-lint-recommended",
		["remark-lint-list-item-indent", "one"],
		["remark-lint-maximum-line-length", false],
		["remark-lint-no-duplicate-headings", false],
		["remark-lint-no-undefined-references", false],
	],
	settings: {
		bullet: "-",
		emphasis: "*",
		strong: "*",
		listItemIndent: "one",
		rule: "-",
		tightDefinitions: true,
	},
};
