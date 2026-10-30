/** One mapping: its trigger (with LEARN), its action, its mode and values. */

import { router, useLocalSearchParams } from "expo-router";
import { useEffect, useState } from "react";
import { Pressable, View } from "react-native";
import type { MappingCommand, MappingRow, MappingScope, MappingTriggerInput } from "@/lib/unit/types";
import { useToast } from "@/state/toast";
import { useClient } from "@/state/unit";
import { useLoad } from "@/state/useLoad";
import { Button, Card, Empty, Row, Rows, Screen, Section, Spinner, T } from "@/ui/kit";
import { usePalette } from "@/ui/theme";

function Chips<K extends string | number>({ options, value, onChange }: { options: { key: K; label: string }[]; value: K | null; onChange: (key: K) => void }) {
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

function Stepper({ label, text, onStep }: { label: string; text: string; onStep: (ticks: number) => void }) {
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

const triggerKind = (row: MappingRow) => (row.trigger.kind === "cc" || row.trigger.kind === "note" ? "midi" : row.trigger.kind);

export default function MappingEditor() {
  const params = useLocalSearchParams<{ scope: MappingScope; id: string }>();
  const scope: MappingScope = params.scope === "global" ? "global" : "preset";
  const client = useClient();
  const toast = useToast();
  const [id, setId] = useState(params.id ?? "");
  const [picking, setPicking] = useState(false);
  const view = useLoad(() => client.mappings(), client);
  const learning = view.data?.learning?.id === id;
  const { reload } = view;

  // An edit or a LEARN can move the row (and change its id): the unit names it in `focus`.
  // Follow it when it changes from one view to the next, as the Remote page does.
  const focus = view.data?.focus;
  const focusKey = focus ? `${focus.scope} ${focus.id}` : "";
  const [seenFocus, setSeenFocus] = useState<string | null>(null);
  if (view.data && focusKey !== seenFocus) {
    setSeenFocus(focusKey);
    if (seenFocus !== null && focus?.scope === scope) setId(focus.id);
  }

  // While LEARN waits, ask once a second, as the Remote page does.
  useEffect(() => {
    if (!learning) return;
    const timer = setInterval(() => void reload(), 1000);
    return () => clearInterval(timer);
  }, [learning, reload]);

  const send = async (command: MappingCommand) => {
    try {
      view.set(await client.mappingCommand(command));
    } catch (error) {
      toast.error(error);
    }
  };

  const close = <Button icon="close" kind="ghost" accessibilityLabel="Close" onPress={() => router.back()} />;
  if (!view.data) return <Screen title="Mapping" right={close}>{view.error ? <Empty title={view.error} /> : <Spinner />}</Screen>;
  const row = view.data.rows[scope].find((r) => r.id === id);
  if (!row) return <Screen title="Mapping" right={close}><Empty icon="midi" title="This mapping is gone" detail="It was removed, here or on the unit." /></Screen>;

  const setTrigger = (trigger: MappingTriggerInput) => void send({ op: "trigger", scope, id, trigger });
  const t = row.trigger;

  return (
    <Screen title={row.triggerLabel} right={close}>
      <Section>Trigger</Section>
      <Chips
        options={view.data.triggers[scope].map((c) => ({ key: c.kind === "extension" ? `ext:${c.key}:${c.trigger}` : c.kind, label: c.label }))}
        value={t.kind === "extension" ? `ext:${t.key}:${t.trigger}` : triggerKind(row)}
        onChange={(key) => {
          const choice = view.data?.triggers[scope].find((c) => (c.kind === "extension" ? `ext:${c.key}:${c.trigger}` : c.kind) === key);
          if (!choice) return;
          // The defaults the Remote page sends when the type changes.
          if (choice.kind === "extension") setTrigger({ kind: "extension", key: choice.key, trigger: choice.trigger });
          else if (choice.kind === "midi") setTrigger({ kind: "cc", number: 20 });
          else if (choice.kind === "stomp") setTrigger({ kind: "stomp", stomp: 0 });
          else if (choice.kind === "expression") setTrigger({ kind: "expression", pedal: 1 });
          else setTrigger({ kind: "presetLoaded" });
        }}
      />
      <Rows>
        {t.kind === "stomp" && (
          <View style={{ padding: 14 }}>
            <Chips options={[..."ABCDEFGH"].map((s, i) => ({ key: i, label: s }))} value={t.stomp} onChange={(stomp) => setTrigger({ kind: "stomp", stomp })} />
          </View>
        )}
        {t.kind === "expression" && (
          <View style={{ padding: 14 }}>
            <Chips options={[{ key: 1, label: "EXP 1" }, { key: 2, label: "EXP 2" }]} value={t.pedal} onChange={(pedal) => setTrigger({ kind: "expression", pedal })} />
          </View>
        )}
        {(t.kind === "cc" || t.kind === "note") && (
          <>
            <View style={{ padding: 14 }}>
              <Chips options={[{ key: "cc", label: "CC" }, { key: "note", label: "Note" }]} value={t.kind} onChange={(kind) => setTrigger({ kind, number: t.number })} />
            </View>
            <Stepper
              label={t.kind === "cc" ? "CC number" : "Note"}
              text={String(t.number)}
              onStep={(d) => setTrigger({ kind: t.kind, number: Math.min(127, Math.max(0, t.number + d)) })}
            />
          </>
        )}
        <Row title={row.triggerLabel} detail={row.triggerDetail} />
      </Rows>
      {row.learns && (
        <Card style={{ padding: 12, flexDirection: "row", alignItems: "center", gap: 10 }}>
          <T size={12.5} style={{ flex: 1 }} tone={learning ? "accent" : "muted"}>
            {learning ? "Learning… send a MIDI message or press a switch." : "LEARN takes the next message the unit gets."}
          </T>
          <Button label={learning ? "Cancel" : "LEARN"} kind={learning ? "ghost" : "accent"} onPress={() => void send({ op: "learn", scope, id, on: !learning })} />
        </Card>
      )}

      <Section>Action</Section>
      <Rows>
        <Row title={row.actionLabel || "Choose an action"} detail={row.actionDetail} chevron onPress={() => setPicking(!picking)} />
      </Rows>
      {picking &&
        view.data.actions[scope].map((group) => (
          <View key={group.label} style={{ gap: 8 }}>
            <T size={11} weight="semibold" tone="muted">
              {group.label.toUpperCase()}
            </T>
            <Rows>
              {group.actions.map((a) => (
                <Row
                  key={a.key}
                  title={a.label}
                  onPress={() => {
                    setPicking(false);
                    void send({ op: "action", scope, id, action: a.key });
                  }}
                />
              ))}
            </Rows>
          </View>
        ))}
      {row.sendMidi && <T size={12} tone="muted">The messages of Send MIDI are edited on the unit.</T>}

      {row.modes.length > 0 && (
        <>
          <Section>Mode</Section>
          <Chips options={row.modes.map((m) => ({ key: m.kind, label: m.label }))} value={row.mode?.kind ?? null} onChange={(mode) => void send({ op: "mode", scope, id, mode })} />
          {row.mode && <T size={12} tone="muted">{row.mode.detail}</T>}
        </>
      )}
      {row.fields.length > 0 && (
        <>
          <Section>Values</Section>
          <Rows>
            {row.fields.map((f) => (
              <Stepper key={f.id} label={f.label} text={f.text} onStep={(ticks) => void send({ op: "step", scope, id, field: f.id, ticks })} />
            ))}
          </Rows>
          {row.summary ? <T size={12} tone="muted" center>{row.summary}</T> : null}
        </>
      )}
      <Button
        label="Remove mapping"
        icon="trash"
        kind="ghost"
        onPress={() =>
          void client.mappingCommand({ op: "remove", scope, id }).then(
            () => router.back(),
            toast.error,
          )
        }
      />
    </Screen>
  );
}
