/** One short message at the bottom of the screen: what the unit refused, or what went through. */

import { type ReactNode, createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { Animated, Pressable, StyleSheet } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { T } from "@/ui/kit";
import { usePalette } from "@/ui/theme";

type Toast = { text: string; tone: "error" | "ok" };
const ToastContext = createContext<(text: string, tone?: Toast["tone"]) => void>(() => {});

export const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toast, setToast] = useState<Toast | null>(null);
  const [opacity] = useState(() => new Animated.Value(0));
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const palette = usePalette();
  const insets = useSafeAreaInsets();

  const show = useCallback(
    (text: string, tone: Toast["tone"] = "error") => {
      clearTimeout(timer.current);
      setToast({ text, tone });
      Animated.timing(opacity, { toValue: 1, duration: 160, useNativeDriver: true }).start();
      timer.current = setTimeout(
        () => Animated.timing(opacity, { toValue: 0, duration: 220, useNativeDriver: true }).start(() => setToast(null)),
        tone === "error" ? 4500 : 2200,
      );
    },
    [opacity],
  );
  useEffect(() => () => clearTimeout(timer.current), []);

  return (
    <ToastContext.Provider value={show}>
      {children}
      {toast && (
        <Animated.View pointerEvents="box-none" style={[styles.wrap, { bottom: insets.bottom + 90, opacity }]}>
          <Pressable
            onPress={() => setToast(null)}
            accessibilityRole="alert"
            style={[
              styles.toast,
              { backgroundColor: palette.panel, borderColor: toast.tone === "error" ? palette.danger : palette.accent },
            ]}
          >
            <T size={13} weight="medium">
              {toast.text}
            </T>
          </Pressable>
        </Animated.View>
      )}
    </ToastContext.Provider>
  );
}

/** `toast(text)`, and `toast.error(e)` for whatever a call threw. */
export function useToast() {
  const show = useContext(ToastContext);
  return useMemo(() => ({ ok: (text: string) => show(text, "ok"), error: (error: unknown) => show(errorText(error), "error") }), [show]);
}

const styles = StyleSheet.create({
  wrap: { position: "absolute", left: 16, right: 16, alignItems: "center" },
  toast: { borderRadius: 10, borderWidth: 1, paddingHorizontal: 14, paddingVertical: 10, maxWidth: 480 },
});
