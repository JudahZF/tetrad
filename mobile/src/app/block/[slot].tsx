/** The block editor: every parameter as a knob, values and bypass per scene, replace and remove. */

import { router, useLocalSearchParams } from "expo-router";
import { useState } from "react";
import { Alert, Platform, View } from "react-native";
import { CATEGORIES, blockAt, liveBypass, liveValue, parseSlot } from "@/lib/preset/model";
import { useUnit } from "@/state/unit";
import { BlockTile } from "@/ui/BlockTile";
import { Knob, formatValue } from "@/ui/Knob";
import { Button, Card, Empty, Row, Rows, Screen, Segmented, T, Toggle } from "@/ui/kit";

function confirm(title: string, detail: string, run: () => void) {
  if (Platform.OS === "web") {
    if (globalThis.confirm?.(`${title}\n${detail}`) ?? true) run();
    return;
  }
  Alert.alert(title, detail, [
    { text: "Cancel", style: "cancel" },
    { text: title, style: "destructive", onPress: run },
  ]);
}

export default function BlockEditor() {
  const params = useLocalSearchParams<{ slot: string }>();
  const slot = parseSlot(params.slot);
  const { preset, edit } = useUnit();
  const [tab, setTab] = useState<"params" | "scenes">("params");
  const block = preset && slot ? blockAt(preset, slot) : null;

  if (!preset || !slot || !block) {
    return (
      <Screen title="Block" right={<Button icon="close" kind="ghost" onPress={() => router.back()} />}>
        <Empty icon="grid" title="This slot is empty now" detail="The block moved or was removed." />
      </Screen>
    );
  }

  const scene = preset.scene;
  const letter = "ABCDEFGH"[scene] ?? "A";
  const bypassed = liveBypass(block, scene);

  return (
    <Screen
      title={
        <View style={{ flexDirection: "row", alignItems: "center", gap: 12 }}>
          <BlockTile category={block.category} size={44} bypassed={bypassed} />
          <View style={{ flex: 1, minWidth: 0 }}>
            <T size={18} weight="bold" lines={1}>
              {block.name}
            </T>
            <T size={12} tone="muted" lines={1}>
              {`${CATEGORIES[block.category].name} · Row ${slot.row + 1} · Column ${slot.column + 1}`}
            </T>
          </View>
          <Toggle value={!bypassed} onChange={(on) => edit({ kind: "bypass", at: slot, bypass: !on })} />
        </View>
      }
      right={<Button icon="close" kind="ghost" onPress={() => router.back()} />}
    >
      {block.description && (
        <T size={12} tone="muted">
          {block.extension ? `${block.description} · ${block.extension}` : block.description}
        </T>
      )}
      <Segmented
        options={[
          { key: "params", label: "Parameters" },
          { key: "scenes", label: "Scenes" },
        ]}
        value={tab}
        onChange={setTab}
      />
      {tab === "params" ? (
        <Card style={{ paddingVertical: 16, paddingHorizontal: 8 }}>
          <View style={{ flexDirection: "row", flexWrap: "wrap", rowGap: 14 }}>
            {block.params.map((param, i) => (
              <View key={param.id} style={{ width: "33.33%", alignItems: "center" }}>
                <Knob
                  param={param}
                  value={liveValue(param, scene)}
                  highlight={i === 0}
                  {...(param.scenes ? { badge: letter } : {})}
                  onChange={(value) => edit({ kind: "param", at: slot, param: param.id, value })}
                />
              </View>
            ))}
          </View>
          {block.params.length === 0 && <T tone="muted" center>This block has no parameters.</T>}
        </Card>
      ) : (
        <Rows>
          <Row
            title="Bypass per scene"
            detail={
              block.sceneBypass
                ? block.sceneBypass.map((b, i) => `${"ABCDEFGH"[i]} ${b ? "off" : "on"}`).join(" · ")
                : "The block is on or off in every scene"
            }
            right={<Toggle value={!!block.sceneBypass} onChange={(on) => edit({ kind: "bypass-scenes", at: slot, on })} />}
          />
          {block.params.map((param) => (
            <Row
              key={param.id}
              title={`${param.name} per scene`}
              detail={
                param.scenes
                  ? param.scenes.map((v, i) => `${"ABCDEFGH"[i]} ${formatValue(param, v)}`).join(" · ")
                  : `${formatValue(param, param.value)} in every scene`
              }
              right={<Toggle value={!!param.scenes} onChange={(on) => edit({ kind: "param-scenes", at: slot, param: param.id, on })} />}
            />
          ))}
        </Rows>
      )}
      <T size={11.5} tone="faint" center>
        {`Changes go to scene ${letter} when a value or the bypass is per scene.`}
      </T>
      <View style={{ flexDirection: "row", gap: 8 }}>
        <Button
          label="Replace"
          icon="swap"
          kind="ghost"
          style={{ flex: 1 }}
          onPress={() => router.replace({ pathname: "/add/[slot]", params: { slot: `${slot.row}:${slot.column}` } })}
        />
        <Button
          label="Remove"
          icon="trash"
          kind="ghost"
          style={{ flex: 1 }}
          onPress={() =>
            confirm("Remove", `${block.name} leaves the Grid. Save the preset on the unit or here to keep the change.`, () => {
              edit({ kind: "remove", at: slot });
              router.back();
            })
          }
        />
      </View>
    </Screen>
  );
}
