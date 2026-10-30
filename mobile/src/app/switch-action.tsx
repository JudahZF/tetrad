/** What a Custom-mode switch does: a system action, a block's bypass or parameter, a preset, MIDI, or an extension command. */

import { router, useLocalSearchParams } from "expo-router";
import { useState } from "react";
import { View } from "react-native";
import type { LayoutScope, SlotActionInput, SystemGroup } from "@/lib/unit/types";
import { SLOT_NAMES } from "@/lib/unit/types";
import { useToast } from "@/state/toast";
import { useClient } from "@/state/unit";
import { useLoad } from "@/state/useLoad";
import { Button, Empty, Row, Rows, Screen, Section, Spinner } from "@/ui/kit";

export default function SwitchAction() {
  const params = useLocalSearchParams<{ scope: LayoutScope; page: string; slot: string }>();
  const client = useClient();
  const toast = useToast();
  const view = useLoad(() => client.footswitch(), client);
  const [setlist, setSetlist] = useState<string | null>(null);
  const presets = useLoad(
    () => (setlist ? client.footswitchCommand({ op: "presets", setlist }) : Promise.resolve(null)),
    client,
    setlist,
  );
  const scope = params.scope === "preset" ? "preset" : "global";
  const page = Number(params.page);
  const slot = Number(params.slot);

  const choose = async (action: SlotActionInput) => {
    try {
      await client.footswitchCommand({ op: "slot", scope, page, slot, edit: { kind: "action", action } });
      router.back();
    } catch (error) {
      toast.error(error);
    }
  };

  const close = <Button icon="close" kind="ghost" accessibilityLabel="Close" onPress={() => router.back()} />;
  if (!view.data) return <Screen title="Action" right={close}>{view.error ? <Empty title={view.error} /> : <Spinner />}</Screen>;
  if (!view.data.mode) return <Screen title="Action" right={close}><Empty title="No Custom footswitch mode" /></Screen>;
  const fs = view.data;
  const groups = new Map<SystemGroup, typeof fs.systems>();
  for (const s of fs.systems) groups.set(s.group, [...(groups.get(s.group) ?? []), s]);
  const listed = presets.data && presets.data.mode && "presets" in presets.data ? presets.data.presets : undefined;

  return (
    <Screen title={`Switch ${SLOT_NAMES[slot] ?? ""} · page ${page + 1}`} right={close}>
      {[...groups].map(([group, items]) => (
        <View key={group} style={{ gap: 12 }}>
          <Section>{group}</Section>
          <Rows>
            {items.map((s) => (
              <Row key={s.label} title={s.label} onPress={() => void choose({ kind: "system", action: s.action })} />
            ))}
          </Rows>
        </View>
      ))}
      {fs.blocks.length > 0 && <Section>Blocks of this preset</Section>}
      {fs.blocks.map((b) => (
        <Rows key={`${b.row}:${b.column}`}>
          <Row title={`${b.name} · Bypass`} detail={b.location} onPress={() => void choose({ kind: "block", row: b.row, column: b.column, parameter: null })} />
          {b.parameters.map((p) => (
            <Row key={p.index} title={`${b.name} · ${p.name}`} detail="Sets the parameter on a press" onPress={() => void choose({ kind: "block", row: b.row, column: b.column, parameter: p.index })} />
          ))}
        </Rows>
      ))}
      <Section>Presets</Section>
      <Rows>
        {fs.presetLoaded && <Row icon="folder" title={`Load ${fs.preset}`} detail="The preset loaded now" onPress={() => void choose({ kind: "loadPreset" })} />}
        {fs.setlists.map((name) => (
          <Row key={name} icon="folder" title={name} detail="Choose a preset of this setlist" chevron onPress={() => setSetlist(setlist === name ? null : name)} />
        ))}
      </Rows>
      {setlist && (listed ? (
        <Rows>
          {listed.slots.map((p) => (
            <Row key={p.slot} title={p.label} onPress={() => void choose({ kind: "preset", setlist, slot: p.slot })} />
          ))}
        </Rows>
      ) : <Spinner />)}
      <Section>MIDI</Section>
      <Rows>
        <Row icon="midi" title="Send MIDI" detail="Its messages are set on the unit" onPress={() => void choose({ kind: "sendMidi" })} />
      </Rows>
      {fs.commands.length > 0 && (
        <>
          <Section>Extension commands</Section>
          <Rows>
            {fs.commands.map((c) => (
              <Row key={`${c.key}/${c.command}`} icon="plugin" title={c.label} onPress={() => void choose({ kind: "command", key: c.key, command: c.command })} />
            ))}
          </Rows>
        </>
      )}
    </Screen>
  );
}
