import { useState } from "react";
import { Modal, Pressable, StyleSheet, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { Icon } from "./Icon";
import { T } from "./kit";
import { usePalette } from "./theme";

const KEYS = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "", "0", "back"] as const;

/** The 4-digit PIN sheet: four boxes and a keypad, as the unit's own PIN entry. */
export function PinPad({
  visible,
  title,
  hint,
  busy,
  error,
  onPin,
  onCancel,
}: {
  visible: boolean;
  title: string;
  hint: string;
  busy?: boolean;
  error?: string | null;
  onPin: (pin: string) => void;
  onCancel: () => void;
}) {
  const palette = usePalette();
  const insets = useSafeAreaInsets();
  const [entry, setEntry] = useState({ pin: "", for: "" });
  // A new opening or a new error starts an empty PIN.
  const round = `${visible}:${error ?? ""}`;
  const pin = entry.for === round ? entry.pin : "";
  const setPin = (update: (p: string) => string) => setEntry({ pin: update(pin), for: round });

  const press = (key: (typeof KEYS)[number]) => {
    if (busy) return;
    if (key === "back") return setPin((p) => p.slice(0, -1));
    if (!key) return;
    const next = (pin + key).slice(0, 4);
    // Four digits go to the unit; the boxes clear for the next try.
    setPin(() => (next.length === 4 ? "" : next));
    if (next.length === 4) onPin(next);
  };

  return (
    <Modal visible={visible} transparent animationType="slide" onRequestClose={onCancel}>
      <Pressable style={styles.scrim} onPress={onCancel} accessibilityLabel="Cancel" />
      <View style={[styles.sheet, { backgroundColor: palette.panel, paddingBottom: insets.bottom + 20 }]}>
        <View style={[styles.grab, { backgroundColor: palette.faint }]} />
        <T size={17} weight="bold" center>
          {title}
        </T>
        <T size={12.5} tone="muted" center style={{ marginTop: 4 }}>
          {hint}
        </T>
        <View style={styles.boxes} accessibilityLabel={`PIN, ${pin.length} of 4 digits`}>
          {[0, 1, 2, 3].map((i) => (
            <View
              key={i}
              style={[
                styles.box,
                { backgroundColor: palette.canvas, borderColor: i === pin.length ? palette.accent : palette.section },
              ]}
            >
              <T size={26} weight="semibold" mono>
                {pin[i] ?? ""}
              </T>
            </View>
          ))}
        </View>
        {error ? (
          <T size={13} tone="danger" center>
            {error}
          </T>
        ) : (
          <T size={13} tone="faint" center>
            {busy ? "Checking…" : " "}
          </T>
        )}
        <View style={styles.keys}>
          {KEYS.map((key, i) => (
            <Pressable
              key={i}
              onPress={() => press(key)}
              disabled={!key}
              accessibilityRole="button"
              accessibilityLabel={key === "back" ? "Delete" : key}
              style={({ pressed }) => [
                styles.key,
                { backgroundColor: key && key !== "back" ? (pressed ? palette.faint : palette.section) : "transparent" },
              ]}
            >
              {key === "back" ? <Icon name="backspace" size={26} /> : <T size={20} weight="medium">{key}</T>}
            </Pressable>
          ))}
        </View>
      </View>
    </Modal>
  );
}

const styles = StyleSheet.create({
  scrim: { flex: 1, backgroundColor: "#000a" },
  sheet: { borderTopLeftRadius: 22, borderTopRightRadius: 22, paddingTop: 10, paddingHorizontal: 16 },
  grab: { width: 38, height: 5, borderRadius: 3, alignSelf: "center", marginBottom: 12 },
  boxes: { flexDirection: "row", gap: 10, justifyContent: "center", marginTop: 14, marginBottom: 8 },
  box: { width: 52, height: 62, borderRadius: 10, borderWidth: 1.5, alignItems: "center", justifyContent: "center" },
  keys: { flexDirection: "row", flexWrap: "wrap", gap: 8, marginTop: 12, maxWidth: 480, alignSelf: "center", width: "100%" },
  key: { width: "31.8%", height: 48, borderRadius: 9, alignItems: "center", justifyContent: "center" },
});
