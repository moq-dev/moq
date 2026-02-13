# Element Wrappers — Code Generation

Generates typed framework wrappers from [Custom Elements Manifest](https://custom-elements-manifest.open-wc.org/) (CEM).

**Input:** `custom-elements.json` + JSDoc annotations → **Output:** Framework components with full TypeScript support

## Architecture

```text
element-wrappers/
├── index.ts              # Orchestrator — runs all enabled generators
├── generators/
│   └── react.ts          # React wrapper generator (forwardRef + JSX types)
└── utils/
    ├── manifest.ts       # CEM loader + JSDoc metadata extraction
    ├── codegen.ts        # JSDoc generation + code formatting
    └── types.ts          # TypeScript interfaces for CEM structures
```

## How It Works

### 1. CEM Analysis

```bash
bunx cem analyze --config cem.config.js
```

Generates `custom-elements.json` from your Web Component source files.

### 2. JSDoc Enhancement

The loader scans all `.ts`/`.tsx` files in `src/` and extracts:

| Annotation     | Purpose                              |
| -------------- | ------------------------------------ |
| `@tag`         | Custom element tag name              |
| `@summary`     | Brief one-line description           |
| `@description` | Detailed description                 |
| `@attr`        | Attribute with type and description  |
| `@slot`        | Slot definition                      |
| `@example`     | Labeled code example (HTML, React…)  |

Example:

```typescript
/**
 * @tag my-player
 * @summary Video player component
 * @description Renders live or on-demand video over MOQ.
 *
 * @attr {string} url - Relay server URL
 * @attr {boolean} muted - Mute audio
 *
 * @example HTML
 * ```html
 * <my-player url="https://relay.example.com" muted></my-player>
 * ```
 *
 * @example React
 * ```tsx
 * <MyPlayer url="https://relay.example.com" muted />
 * ```
 */
export class MyPlayerElement extends HTMLElement { }
```

### 3. Wrapper Generation

Each enabled generator creates framework-specific code:

- **React** — `React.forwardRef` components, typed props from CEM attributes, `JSX.IntrinsicElements` augmentation

## Usage

Add to your package's `package.json`:

```json
{
  "scripts": {
    "prebuild": "bunx cem analyze && bun ../scripts/element-wrappers/index.ts"
  }
}
```

Output: `src/wrappers/<framework>/index.ts` (auto-generated, add to `.gitignore`)

## Adding a New Framework

### 1. Create the Generator

Create `generators/<framework>.ts`:

```typescript
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { formatCode, generateJSDoc } from "../utils/codegen";
import { extractCustomElements, loadManifest, tagNameToComponentName } from "../utils/manifest";
import type { CustomElement } from "../utils/types";

function generateComponent(element: CustomElement): string {
  const name = tagNameToComponentName(element.tagName);
  const jsDoc = generateJSDoc(
    element.summary, element.description,
    element.slots, element.events,
    element.attributes, element.properties,
    element.examples,
  );
  // Return framework-specific component code
  return `${jsDoc}\nexport const ${name} = /* ... */;\n`;
}

export function generateFrameworkWrappers(basePath: string = process.cwd()): void {
  console.log("\n🔧 Generating <Framework> wrappers...");

  const manifest = loadManifest(basePath);
  const elements = extractCustomElements(manifest);
  if (elements.length === 0) return;

  const output = elements.map(generateComponent).join("\n");
  const outputDir = join(basePath, "src", "wrappers", "<framework>");

  mkdirSync(outputDir, { recursive: true });
  writeFileSync(join(outputDir, "index.ts"), formatCode(output));

  console.log(`✅ Generated ${elements.length} wrappers`);
}
```

### 2. Register It

In `index.ts`:

```typescript
import { generateFrameworkWrappers } from "./generators/<framework>";

const generators: Generator[] = [
  { name: "React", fn: generateReactWrappers, enabled: true },
  { name: "<Framework>", fn: generateFrameworkWrappers, enabled: true },
];
```

### 3. Export from Package

In the consuming package's `package.json`:

```json
{
  "exports": {
    "./<framework>": {
      "types": "./wrappers/<framework>/index.d.ts",
      "default": "./src/wrappers/<framework>/index.ts"
    }
  }
}
```

## Utilities

| Function                            | Description                                    |
| ----------------------------------- | ---------------------------------------------- |
| `loadManifest(basePath)`            | Loads CEM + enhances with JSDoc metadata       |
| `extractCustomElements(manifest)`   | Extracts custom element definitions from CEM   |
| `tagNameToComponentName(tagName)`   | `kebab-case` → `PascalCase` (e.g. `my-el` → `MyEl`) |
| `generateJSDoc(...)`                | Creates JSDoc comment blocks from metadata     |
| `formatCode(code)`                  | Ensures consistent formatting                  |

## Troubleshooting

| Problem                        | Solution                                                  |
| ------------------------------ | --------------------------------------------------------- |
| "No custom elements found"     | Run `cem analyze` first; ensure `@tag` annotations exist  |
| "Failed to load manifest"      | Check `custom-elements.json` exists and is valid JSON     |
| Missing types in wrappers      | Verify JSDoc `@attr` annotations have `{type}` specified  |
