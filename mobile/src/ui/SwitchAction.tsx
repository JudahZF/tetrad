/** One action of a Custom footswitch switch, edited as a Mappings row (the Control tab). */

import { View } from "react-native";
import { CATEGORIES } from "@/lib/preset/model";
import type { SlotActionRow, SlotEdit } from "@/lib/unit/types";
import { useUnit } from "@/state/unit";
import { BlockTile } from "@/ui/BlockTile";
import { Row, Rows, T } from "@/ui/kit";
import { usePalette } from "@/ui/theme";
import { Chips, Stepper } from "@/ui/Values";

/** One action of a switch, edited as a Mappings row: what it does, its mode and its values. */
export function SwitchAction({ item, index, onChoose, onEdit }: { item: SlotActionRow; index: number; onChoose: () => void; onEdit: (edit: SlotEdit) => void }) {
  const { preset } = useUnit();
  const palette = usePalette();
  const action = item.action;
  const block = action.kind === "block" ? preset?.grid[action.row]?.[action.column] : null;
  return (
    <Rows>
      <Row
        lead={
          block ? <BlockTile category={block.category} size={30} /> : (
            <View style={{ width: 30, height: 30, borderRadius: 6, backgroundColor: palette.canvas, alignItems: "center", justifyContent: "center" }}>
              <T size={13} weight="semibold">{String(index + 1)}</T>
            </View>
          )
        }
        title={item.label}
        detail={[item.available ? item.detail || (block ? CATEGORIES[block.category].name : "") : item.detail, item.summary].filter(Boolean).join(" · ")}
        chevron
        onPress={onChoose}
      />
      {item.modes.length > 0 && (
        <View style={{ padding: 14, gap: 8 }}>
          <Chips options={item.modes.map((m) => ({ key: m.kind, label: m.label }))} value={item.mode?.kind ?? null} onChange={(mode) => onEdit({ kind: "mode", index, mode })} />
          {item.mode && <T size={12} tone="muted">{item.mode.detail}</T>}
        </View>
      )}
      {item.fields.map((f) => (
        <Stepper key={f.id} label={f.label} text={f.text} onStep={(ticks) => onEdit({ kind: "step", index, field: f.id, ticks })} />
      ))}
      {action.kind === "sendMidi" && <Row title="Messages" detail={`${item.messages.length} · edit them on the unit or the Remote page`} />}
      <Row icon="trash" title="Remove action" onPress={() => onEdit({ kind: "remove", index })} />
    </Rows>
  );
}
