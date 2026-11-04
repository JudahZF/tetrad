/** The mode chips and value steppers of a Mappings row, also used by a Custom switch action. */

import { Pressable, View } from "react-native";
import { Button, Row, T } from "@/ui/kit";
import { usePalette } from "@/ui/theme";

export function Chips<K extends string | number>({ options, value, onChange }: { options: { key: K; label: string }[]; value: K | null; onChange: (key: K) => void }) {
  const palette = usePalette();
  return (
    <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
      {options.map((o) => {
        const on = o.key === value;
        return (
          <Pressable
            key={String(o.key)}
            onPress={() => onChange(o.key)}
            accessibilityRole="button"
            accessibilityState={{ selected: on }}
            style={{ paddingHorizontal: 12, paddingVertical: 7, borderRadius: 14, backgroundColor: on ? palette.accentFill : palette.surface, borderWidth: 1, borderColor: on ? palette.accent : "transparent" }}
          >
            <T size={12.5} weight="semibold" tone={on ? "accent" : "muted"}>
              {o.label}
            </T>
          </Pressable>
        );
      })}
    </View>
  );
}

/** − and + as on the unit: each tap is one step of the value's own size. */
export function Stepper({ label, text, onStep }: { label: string; text: string; onStep: (ticks: number) => void }) {
  return (
    <Row
      title={label}
      right={
        <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
          <Button icon="minus" accessibilityLabel={`Lower ${label}`} onPress={() => onStep(-1)} />
          <T size={13} mono style={{ minWidth: 64, textAlign: "center" }}>
            {text}
          </T>
          <Button icon="plus" accessibilityLabel={`Raise ${label}`} onPress={() => onStep(1)} />
        </View>
      }
    />
  );
}
