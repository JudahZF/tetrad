/** The palette in use. The app takes the theme the unit uses, so it looks like the unit's own screen. */

import { type ReactNode, createContext, useContext, useMemo, useState } from "react";
import { type Palette, STOCK } from "@/lib/themes";

export { BUILTIN_THEMES, type Palette, STOCK, type Theme, mix } from "@/lib/themes";

export const font = {
  regular: "IBMPlexSans_400Regular",
  medium: "IBMPlexSans_500Medium",
  semibold: "IBMPlexSans_600SemiBold",
  bold: "IBMPlexSans_700Bold",
  mono: "IBMPlexMono_500Medium",
} as const;

interface ThemeState {
  palette: Palette;
  /** Shows a theme before the unit reports one, or as a preview. */
  setPalette: (palette: Palette) => void;
}

const ThemeContext = createContext<ThemeState>({ palette: STOCK, setPalette: () => {} });

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [palette, setPalette] = useState(STOCK);
  const value = useMemo(() => ({ palette, setPalette }), [palette]);
  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

export const usePalette = () => useContext(ThemeContext).palette;
export const useSetPalette = () => useContext(ThemeContext).setPalette;
