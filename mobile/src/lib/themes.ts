/**
 * Palette roles of the unit (crates/tetrad-theme) and its built-in themes.
 */

import type { UnitPalette } from "@/lib/unit/types";

/** The 18 roles of a theme (camelCase, as the unit sends them). */
export type Palette = UnitPalette;

export const STOCK: Palette = {
  background: "#121212", canvas: "#000000", panel: "#2e2e2e", surface: "#1e1e1e",
  rowSelected: "#252525", section: "#404040", divider: "#121212", scrollbar: "#373737",
  text: "#ffffff", muted: "#959595", faint: "#616161", cable: "#c1c1c1",
  accent: "#45f862", accentFill: "#17291a", onAccent: "#000000", action: "#1a3af8",
  danger: "#ff2727", warning: "#ffd236",
};

export interface Theme {
  id: string;
  name: string;
  palette: Palette;
}

/** crates/tetrad-theme/src/builtin.rs `BUILTINS`. */
export const BUILTIN_THEMES: Theme[] = [
  { id: "stock", name: "Stock", palette: STOCK },
  {
    id: "stage-contrast",
    name: "Stage High Contrast",
    palette: {
      background: "#101010", canvas: "#000000", panel: "#2a2a2a", surface: "#1a1a1a",
      rowSelected: "#242424", section: "#3a3a3a", divider: "#101010", scrollbar: "#4a4a4a",
      text: "#ffffff", muted: "#c8c8c8", faint: "#8c8c8c", cable: "#ffffff",
      accent: "#ffe100", accentFill: "#1a1700", onAccent: "#000000", action: "#2f6bff",
      danger: "#ff3b3b", warning: "#ff9f1a",
    },
  },
  {
    id: "slate",
    name: "Slate",
    palette: {
      background: "#10151b", canvas: "#090c10", panel: "#26303b", surface: "#1a222b",
      rowSelected: "#202932", section: "#354251", divider: "#10151b", scrollbar: "#2f3a46",
      text: "#eef3f8", muted: "#9aa8b6", faint: "#627080", cable: "#b8c4d0",
      accent: "#5aa9ff", accentFill: "#14243a", onAccent: "#000000", action: "#3b5bdb",
      danger: "#ff5c5c", warning: "#ffd166",
    },
  },
  {
    id: "ember",
    name: "Ember",
    palette: {
      background: "#15110e", canvas: "#0b0806", panel: "#2f2621", surface: "#211a16",
      rowSelected: "#29211c", section: "#42362f", divider: "#15110e", scrollbar: "#3a2f28",
      text: "#fff6ee", muted: "#b8a596", faint: "#75665b", cable: "#d6c4b5",
      accent: "#ffa53a", accentFill: "#2e1f10", onAccent: "#000000", action: "#c4471c",
      danger: "#ff4d4d", warning: "#ffd23f",
    },
  },
];

/** `colour` over `base` at `amount` (0–1), as CSS `color-mix` in sRGB. */
export function mix(colour: string, base: string, amount: number): string {
  const rgb = (hex: string) => [1, 3, 5].map((i) => Number.parseInt(hex.slice(i, i + 2), 16));
  const [a, b] = [rgb(colour), rgb(base)];
  const out = a.map((v, i) => Math.round(v * amount + (b[i] ?? 0) * (1 - amount)));
  return `#${out.map((v) => v.toString(16).padStart(2, "0")).join("")}`;
}

