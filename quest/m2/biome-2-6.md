# [S] Upgrade Biome to 2.6

## Goal

The workspace runs Biome 2.6.4 or later with `just check` passing and no file
reformatted by the upgrade itself. The YAML and Markdown formatters it adds
stay off until their own quests turn them on.

## Plan

Follow [moq-dev/moq.pro#3109](https://github.com/moq-dev/moq.pro/pull/3109),
which made the same move from 2.5.15: Biome 2.6 rejects config that names
rules under their old groups (`noPlaywrightWaitForTimeout` moved from
`nursery` to `suspicious`), turns on its YAML and Markdown formatters by
default, and adds lint rules (`useTemplate`, `useMathMinMax`,
`useDomNodeTextContent`) whose autofixes need review; `useMathMinMax` there
would have passed `Date`s to `Math.max`. Set `yaml.formatter.enabled` and
`markdown.formatter.enabled` to false, and confirm they are honored, since
2.6.3 was reported to ignore the YAML one. Run the whole-workspace
`biome check`, not only CI's scoped one.

Decided with the maintainer on 2026-10-29: mirror moq.pro's Biome 2.6 move
upstream (✅ yes), in m2.

## Related

- [Format YAML with Biome](/quest/m2/biome-yaml.md) - turns on the YAML
  formatter this keeps off
- [Format Markdown with Biome](/quest/m2/biome-markdown.md) - turns on the
  Markdown formatter and retires remark
