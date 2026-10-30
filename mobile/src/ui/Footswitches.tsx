import { Pressable, View } from "react-native";
import { SLOT_NAMES, SLOT_ORDER, type SlotView } from "@/lib/unit/types";
import { T } from "./kit";
import { mix, usePalette } from "./theme";

/**
 * The switches as the unit's Custom editor draws them: A B C D Down on top, E F G H Tap below.
 * A tile is a dark tint of its colour, full colour when its switch is on now; the letter sits
 * top right and the label bottom left.
 */
export function FootswitchTiles({
  slots,
  selected,
  onSelect,
}: {
  slots: SlotView[];
  selected: number | null;
  onSelect: (slot: number) => void;
}) {
  const palette = usePalette();
  const tile = (index: number) => {
    const slot = slots[index];
    const name = SLOT_NAMES[index];
    const system = index >= 8;
    const colour = slot?.action ? slot.colour : "#3a3a3a";
    const lit = !!slot?.engaged && !system;
    const on = selected === index;
    return (
      <Pressable
        key={index}
        onPress={() => onSelect(index)}
        accessibilityRole="button"
        accessibilityLabel={`Switch ${name}: ${slot?.label || "empty"}`}
        accessibilityState={{ selected: on }}
        style={{
          flex: 1,
          height: 64,
          borderRadius: 6,
          padding: 6,
          justifyContent: "flex-end",
          backgroundColor: lit ? colour : system || !slot?.action ? "#3a3a3a" : mix(colour, palette.surface, 0.18),
          borderWidth: 2,
          borderColor: on ? palette.accent : "transparent",
          opacity: slot && !slot.available ? 0.5 : 1,
        }}
      >
        <T size={9.5} weight="semibold" color={lit ? "#0009" : palette.muted} style={{ position: "absolute", top: 4, right: 6 }}>
          {name}
        </T>
        <T size={10.5} weight="semibold" color={lit ? "#000" : palette.text} lines={2} style={{ lineHeight: 12 }}>
          {slot?.label ?? ""}
        </T>
      </Pressable>
    );
  };
  return (
    <View style={{ gap: 5 }}>
      {[SLOT_ORDER.slice(0, 5), SLOT_ORDER.slice(5)].map((row, r) => (
        <View key={r} style={{ flexDirection: "row", gap: 5 }}>
          {row.map(tile)}
        </View>
      ))}
    </View>
  );
}

/** The 8 LED colours, and "auto" (the action's own colour). */
export function ColourDots({ colours, value, onChange }: { colours: string[]; value: number | null; onChange: (index: number | null) => void }) {
  const palette = usePalette();
  const ring = (on: boolean) => ({ borderWidth: 2, borderColor: on ? palette.accent : "transparent", padding: 2, borderRadius: 14 });
  return (
    <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between" }}>
      <Pressable onPress={() => onChange(null)} style={ring(value === null)} accessibilityLabel="Automatic colour" accessibilityState={{ selected: value === null }}>
        <View style={{ width: 20, height: 20, borderRadius: 10, backgroundColor: palette.canvas, alignItems: "center", justifyContent: "center" }}>
          <T size={9} weight="bold" tone="muted">A</T>
        </View>
      </Pressable>
      {colours.map((c, i) => (
        <Pressable key={i} onPress={() => onChange(i)} style={ring(value === i)} accessibilityLabel={`Colour ${i + 1}`} accessibilityState={{ selected: value === i }}>
          <View style={{ width: 20, height: 20, borderRadius: 10, backgroundColor: c }} />
        </Pressable>
      ))}
    </View>
  );
}
