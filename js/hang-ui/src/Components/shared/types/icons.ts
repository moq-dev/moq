import type { JSX } from "solid-js/jsx-runtime";

export type SVGIcons = (() => JSX.Element) | Record<PropertyKey, () => JSX.Element>;

// Generic icon set types for button icons
export type IconSet = Record<PropertyKey, () => JSX.Element>;
export type Icon = () => JSX.Element;
