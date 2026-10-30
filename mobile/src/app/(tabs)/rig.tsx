/** Rig: the loaded preset, its scenes and the Grid, live from the unit. */

import { router } from "expo-router";
import { useState } from "react";
import { View } from "react-native";
import { CATEGORIES, type Slot, blockAt, liveBypass, liveValue, slotKey } from "@/lib/preset/model";
import { useUnit } from "@/state/unit";
import { BlockTile } from "@/ui/BlockTile";
import { Grid, SceneStrip } from "@/ui/Grid";
import { Knob } from "@/ui/Knob";
import { Button, Card, Empty, Screen, Spinner, T, Toggle } from "@/ui/kit";
import { usePalette } from "@/ui/theme";

export default function Rig() {
  const { client, preset, edit, save, sending } = useUnit();
  const palette = usePalette();
  const [selected, setSelected] = useState<Slot | null>({ row: 0, column: 2 });
  const [copying, setCopying] = useState<Slot | null>(null);

  if (!client?.has("preset")) {
    return (
      <Screen title="Rig">
        <Empty
          icon="grid"
          title="Grid editing needs a newer Tetrad"
          detail={
            client?.transport === "wifi"
              ? "Remote over Wi-Fi does not share the preset. Grid editing works over Bluetooth with Tetrad Link protocol 2."
              : "This Tetrad's Bluetooth link does not share the preset yet (it needs Link protocol 2). The Library, Control and Unit tabs work."
          }
        />
      </Screen>
    );
  }
  if (!preset) {
    return (
      <Screen title="Rig">
        <Spinner />
      </Screen>
    );
  }

  const block = selected ? blockAt(preset, selected) : null;
  const sceneLetter = "ABCDEFGH"[preset.scene] ?? "A";

  const header = (
    <View style={{ flexDirection: "row", alignItems: "center", gap: 10 }}>
      <View style={{ backgroundColor: palette.warning, borderRadius: 5, paddingHorizontal: 6, paddingVertical: 2 }}>
        <T size={12} mono color="#000">
          {preset.place}
        </T>
      </View>
      <View style={{ flex: 1, minWidth: 0 }}>
        <T size={19} weight="bold" lines={1}>
          {preset.name}
        </T>
        <T size={11} tone={preset.edited ? "warning" : "muted"}>
          {sending ? "Sending…" : preset.edited ? "● Edited · not saved" : `Scene ${sceneLetter} · ${preset.scenes[preset.scene]?.name ?? ""}`}
        </T>
      </View>
    </View>
  );

  return (
    <Screen title={header} right={<Button label="Save" icon="save" kind="action" disabled={!preset.edited || sending} onPress={() => void save()} />} scroll={false}>
      <SceneStrip preset={preset} onScene={(scene) => edit({ kind: "scene", scene })} />
      <Grid
        preset={preset}
        selected={selected}
        copying={copying}
        onSelect={(slot) => {
          setCopying(null);
          if (selected && slotKey(selected) === slotKey(slot)) router.push({ pathname: "/block/[slot]", params: { slot: slotKey(slot) } });
          else setSelected(slot);
        }}
        onEmpty={(slot) => {
          if (copying) {
            edit({ kind: "copy", from: copying, to: slot });
            setCopying(null);
            setSelected(slot);
          } else router.push({ pathname: "/add/[slot]", params: { slot: slotKey(slot) } });
        }}
        onMove={(from, to) => {
          edit({ kind: "move", from, to });
          setSelected(to);
        }}
        onHold={(slot) => {
          setSelected(slot);
          setCopying(slot);
        }}
      />
      {copying ? (
        <Card style={{ padding: 12, flexDirection: "row", alignItems: "center", gap: 10, backgroundColor: palette.accentFill, borderWidth: 1, borderColor: palette.accent }}>
          <T size={12.5} style={{ flex: 1 }}>
            Tap an empty slot to copy <T size={12.5} weight="bold">{blockAt(preset, copying)?.name ?? "the block"}</T>
          </T>
          <Button label="Cancel" kind="ghost" onPress={() => setCopying(null)} />
        </Card>
      ) : (
        <T size={11.5} tone="muted" center>
          Tap to select, again to open · Drag to move · Hold to copy · Tap + to add
        </T>
      )}
      {block && selected ? (
        <Card style={{ padding: 12, gap: 6 }}>
          <View style={{ flexDirection: "row", alignItems: "center", gap: 10 }}>
            <BlockTile category={block.category} size={34} bypassed={liveBypass(block, preset.scene)} />
            <View style={{ flex: 1, minWidth: 0 }}>
              <T size={14} weight="semibold" lines={1}>
                {block.name}
              </T>
              <T size={11} tone="muted">
                {`${CATEGORIES[block.category].name} · Row ${selected.row + 1} · Column ${selected.column + 1}`}
              </T>
            </View>
            <Toggle value={!liveBypass(block, preset.scene)} onChange={(on) => edit({ kind: "bypass", at: selected, bypass: !on })} />
          </View>
          <View style={{ flexDirection: "row", justifyContent: "space-around" }}>
            {block.params.slice(0, 4).map((param, i) => (
              <Knob
                key={param.id}
                param={param}
                value={liveValue(param, preset.scene)}
                size={52}
                highlight={i === 0}
                {...(param.scenes ? { badge: sceneLetter } : {})}
                onChange={(value) => edit({ kind: "param", at: selected, param: param.id, value })}
              />
            ))}
          </View>
          <Button
            label="Open block"
            icon="forward"
            kind="ghost"
            onPress={() => router.push({ pathname: "/block/[slot]", params: { slot: slotKey(selected) } })}
          />
        </Card>
      ) : (
        <Card style={{ padding: 14 }}>
          <T size={13} tone="muted" center>
            Select a block to change its parameters.
          </T>
        </Card>
      )}
    </Screen>
  );
}
