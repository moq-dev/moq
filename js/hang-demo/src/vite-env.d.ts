/// <reference types="vite/client" />

interface ImportMetaEnv {
	readonly VITE_RELAY_URL: string;
}

interface ImportMeta {
	readonly env: ImportMetaEnv;
}

declare const __HANG_UI_ASSETS_PATH__: string;
