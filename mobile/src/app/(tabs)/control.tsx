/** Control: the Custom footswitch mode's switches, and the mappings of the preset and the unit. */

import { router, useFocusEffect } from "expo-router";
import { useCallback, useState } from "react";
import { Pressable, View } from "react-native";
import { CATEGORIES } from "@/lib/preset/model";
import type { FootswitchCommand, FootswitchMode, LayoutScope, MappingScope, MappingsView, SlotEdit, SlotView } from "@/lib/unit/types";
import { SLOT_NAMES } from "@/lib/unit/types";
import { useToast } from "@/state/toast";
import { useClient, useUnit } from "@/state/unit";
import { useLoad } from "@/state/useLoad";
import { BlockTile } from "@/ui/BlockTile";
import { ColourDots, FootswitchTiles } from "@/ui/Footswitches";
import { Icon } from "@/ui/Icon";
import { Button, Card, Empty, Pill, Row, Rows, Screen, Segmented, Spinner, T, Toggle } from "@/ui/kit";
import { TextPrompt } from "@/ui/Sheet";
import { usePalette } from "@/ui/theme";

function Footswitches() {
  const client = useClient();
  const { preset } = useUnit();
  const toast = useToast();
  const palette = usePalette();
  const view = useLoad(() => client.footswitch(), client);
  const [scope, setScope] = useState<LayoutScope>("global");
  const [page, setPage] = useState(0);
  const [slot, setSlot] = useState<number | null>(3);
  const [labelling, setLabelling] = useState(false);
  const { reload } = view;
  useFocusEffect(useCallback(() => void reload(), [reload]));

  if (!view.data) return view.error ? <Empty icon="footswitch" title="Could not read the switches" detail={view.error} /> : <Spinner />;
  if (!view.data.mode) {
    return <Empty icon="footswitch" title="No Custom footswitch mode" detail="Install Custom Footswitch Mode (Unit › Extensions) to set up four pages of switches." />;
  }
  const fs: FootswitchMode = view.data;
  const layout = scope === "global" ? fs.layouts.global : fs.layouts.preset;
  const slots = layout?.[Math.min(page, (layout?.length ?? 1) - 1)] ?? [];
  const current: SlotView | undefined = slot !== null ? slots[slot] : undefined;

  /** True when the unit took the command. */
  const send = async (command: FootswitchCommand) => {
    try {
      view.set(await client.footswitchCommand(command));
      return true;
    } catch (error) {
      toast.error(error);
      return false;
    }
  };
  const editSlot = (edit: SlotEdit) => slot !== null && void send({ op: "slot", scope, page, slot, edit });

  const action = current?.action;
  const block = action?.kind === "block" ? preset?.grid[action.row]?.[action.column] : null;

  return (
    <>
      <View style={{ flexDirection: "row", gap: 8, alignItems: "center" }}>
        <View style={{ flex: 1 }}>
          <Segmented
            options={[
              { key: "global", label: "ALL PRESETS" },
              { key: "preset", label: "THIS PRESET" },
            ]}
            value={scope}
            onChange={setScope}
          />
        </View>
        {layout && (
          <Button
            icon="copy"
            accessibilityLabel="Copy page"
            onPress={() => void send({ op: "copyPage", scope, from: page, to: (page + 1) % fs.mode.pages }).then((done) => done && toast.ok(`Page ${page + 1} copied to page ${((page + 1) % fs.mode.pages) + 1}`))}
          />
        )}
      </View>
      <View style={{ flexDirection: "row", gap: 6 }}>
        {Array.from({ length: fs.mode.pages }, (_, i) => (
          <Pressable
            key={i}
            onPress={() => setPage(i)}
            accessibilityRole="tab"
            accessibilityState={{ selected: i === page }}
            style={{ flex: 1, alignItems: "center", paddingVertical: 8, borderRadius: 7, backgroundColor: i === page ? palette.panel : palette.surface }}
          >
            <T size={11} weight="semibold" tone={i === page ? "accent" : "muted"}>{`PAGE ${i + 1}`}</T>
          </Pressable>
        ))}
      </View>
      {!layout ? (
        <Card style={{ padding: 14, gap: 10 }}>
          <T size={13} tone="muted">
            {fs.presetLoaded ? `${fs.preset} uses the layout of all presets.` : "Load a preset on the unit to give it its own layout."}
          </T>
          {fs.presetLoaded && <Button label="Give this preset its own layout" kind="accent" onPress={() => void send({ op: "presetLayout", own: true })} />}
        </Card>
      ) : (
        <>
          <FootswitchTiles slots={slots} selected={slot} onSelect={setSlot} />
          {current && slot !== null && (
            <Rows>
              <Row
                lead={block ? <BlockTile category={block.category} size={30} /> : <View style={{ width: 30, height: 30, borderRadius: 6, backgroundColor: current.action ? current.colour : palette.canvas, alignItems: "center", justifyContent: "center" }}>{!current.action && <Icon name="plus" size={18} />}</View>}
                title={current.action ? current.label : `Switch ${SLOT_NAMES[slot]} is empty`}
                detail={current.action ? current.detail || (block ? CATEGORIES[block.category].name : "") : "Choose what it does"}
                chevron
                onPress={() => router.push({ pathname: "/switch-action", params: { scope, page: String(page), slot: String(slot) } })}
              />
              {current.action && (
                <View style={{ padding: 14, gap: 12 }}>
                  <Pressable onPress={() => setLabelling(true)} style={{ flexDirection: "row", alignItems: "center" }} accessibilityRole="button">
                    <T size={14} weight="semibold" style={{ flex: 1 }}>Label</T>
                    <T size={14} tone={current.customLabel ? "text" : "muted"}>{current.customLabel ?? `${current.label} (auto)`}</T>
                    <Icon name="pencil" size={16} color={palette.faint} style={{ marginLeft: 8 }} />
                  </Pressable>
                  <ColourDots colours={fs.colours} value={current.swatch} onChange={(colour) => editSlot({ kind: "colour", colour })} />
                </View>
              )}
              {current.momentary !== null && (
                <Row
                  title="Momentary"
                  detail="The block changes only while the switch is down"
                  right={<Toggle value={current.momentary} onChange={(momentary) => editSlot({ kind: "momentary", momentary })} />}
                />
              )}
              {current.value !== null && (
                <Row
                  title="Value"
                  detail="Where the parameter goes on a press"
                  right={
                    <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
                      <Button icon="minus" onPress={() => editSlot({ kind: "value", value: Math.max(0, (current.value ?? 0) - 5) })} />
                      <T size={13} mono style={{ width: 48, textAlign: "center" }}>{`${current.value} %`}</T>
                      <Button icon="plus" onPress={() => editSlot({ kind: "value", value: Math.min(100, (current.value ?? 0) + 5) })} />
                    </View>
                  }
                />
              )}
              {current.action && <Row icon="trash" title="Clear this switch" onPress={() => editSlot({ kind: "clear" })} />}
            </Rows>
          )}
          {scope === "preset" && <Button label="Use the layout of all presets" kind="ghost" onPress={() => void send({ op: "presetLayout", own: false })} />}
        </>
      )}
      <T size={11.5} tone="faint" center>
        {scope === "preset" ? "THIS PRESET is saved with the preset: save it on the unit." : "ALL PRESETS is saved on the unit at once."}
      </T>
      <TextPrompt
        visible={labelling}
        title={`Label of switch ${slot !== null ? SLOT_NAMES[slot] : ""}`}
        initial={current?.customLabel ?? ""}
        hint="At most 16 characters. Empty uses the action's name."
        action="Set label"
        onClose={() => setLabelling(false)}
        onDone={(label) => {
          setLabelling(false);
          editSlot({ kind: "label", label: label.trim() });
        }}
      />
    </>
  );
}

/** The trigger in the row's chip: short, as the unit's list shows it. */
function triggerChip(row: MappingsView["rows"]["preset"][number]) {
  return row.trigger.kind === "presetLoaded" ? "On load" : row.triggerLabel;
}

function Mappings() {
  const client = useClient();
  const toast = useToast();
  const palette = usePalette();
  const [scope, setScope] = useState<MappingScope>("preset");
  const view = useLoad(() => client.mappings(), client);
  const { reload } = view;
  useFocusEffect(useCallback(() => void reload(), [reload]));

  if (!view.data) return view.error ? <Empty icon="midi" title="Could not read the mappings" detail={view.error} /> : <Spinner />;
  const rows = view.data.rows[scope];

  const add = async () => {
    try {
      const next = await client.mappingCommand({ op: "add", scope });
      view.set(next);
      if (next.focus) router.push({ pathname: "/mapping", params: { scope: next.focus.scope, id: next.focus.id } });
    } catch (error) {
      toast.error(error);
    }
  };

  return (
    <>
      <Segmented
        options={[
          { key: "preset", label: "THIS PRESET" },
          { key: "global", label: "GLOBAL" },
        ]}
        value={scope}
        onChange={setScope}
      />
      {view.data.notice && (
        <Card style={{ padding: 12, borderWidth: 1, borderColor: palette.warning }}>
          <T size={12.5}>{view.data.notice}</T>
        </Card>
      )}
      {rows.length === 0 ? (
        <Empty icon="midi" title="No mappings here yet" detail={scope === "preset" ? "Mappings of this preset are saved with the preset." : "Global mappings work in every preset."} />
      ) : (
        <Rows>
          {rows.map((row) => (
            <Row
              key={row.id}
              lead={
                <View style={{ width: 92, backgroundColor: palette.canvas, borderRadius: 6, paddingVertical: 5, paddingHorizontal: 4, alignItems: "center" }}>
                  <T size={11} mono lines={1}>{triggerChip(row)}</T>
                </View>
              }
              title={row.actionLabel || "Choose an action"}
              detail={
                <View style={{ flexDirection: "row", alignItems: "center", gap: 6, marginTop: 2 }}>
                  {row.mode && <Pill label={row.mode.label.toUpperCase()} tone="plain" />}
                  <T size={11.5} tone="muted" lines={1} style={{ flexShrink: 1 }}>{row.summary || row.triggerDetail}</T>
                </View>
              }
              chevron
              onPress={() => router.push({ pathname: "/mapping", params: { scope, id: row.id } })}
            />
          ))}
        </Rows>
      )}
      <Button label="Add mapping" icon="plus" kind="accent" onPress={() => void add()} />
      <T size={11.5} tone="faint" center>
        {scope === "preset" ? "THIS PRESET mappings are part of the preset: save it to keep them." : "GLOBAL mappings are saved on the unit at once."}
      </T>
    </>
  );
}

export default function Control() {
  const client = useClient();
  const [tab, setTab] = useState<"footswitches" | "mappings">("footswitches");
  return (
    <Screen title="Control" right={<Pill label={client.name} tone="plain" />}>
      <Segmented
        options={[
          { key: "footswitches", label: "Footswitches" },
          { key: "mappings", label: "Mappings" },
        ]}
        value={tab}
        onChange={setTab}
      />
      {tab === "footswitches" ? (
        <Footswitches />
      ) : (
        <Mappings />
      )}
    </Screen>
  );
}
