/** Extensions: what is installed (on, off, faulted, updates, settings, commands) and what the catalog offers. */

import { router } from "expo-router";
import { useEffect, useState } from "react";
import { Alert, Platform, View } from "react-native";
import type { ExtensionCommand, InstalledExtension } from "@/lib/unit/types";
import { useToast } from "@/state/toast";
import { useClient } from "@/state/unit";
import { useLoad } from "@/state/useLoad";
import { Button, Card, Empty, Pill, Row, Rows, Screen, Section, Spinner, T, Toggle } from "@/ui/kit";

function ask(title: string, detail: string, action: string, run: () => void) {
  if (Platform.OS === "web") {
    if (globalThis.confirm?.(`${title}\n\n${detail}`) ?? true) run();
    return;
  }
  Alert.alert(title, detail, [
    { text: "Cancel", style: "cancel" },
    { text: action, onPress: run },
  ]);
}

const STATE: Record<InstalledExtension["state"], [string, "ok" | "wait" | "bad" | "plain"]> = {
  running: ["Running", "ok"],
  starting: ["Starting", "wait"],
  off: ["Off", "plain"],
  faulted: ["Faulted", "bad"],
};

export default function Extensions() {
  const client = useClient();
  const toast = useToast();
  const list = useLoad(() => client.extensions(), client);
  const [open, setOpen] = useState<string | null>(null);
  // A repository job or a starting extension: ask again until it settles, as the Remote page does.
  const working = list.data?.busy === true || list.data?.installed.some((e) => e.state === "starting") === true;
  const { reload } = list;
  useEffect(() => {
    if (!working) return;
    const timer = setInterval(() => void reload(), 1500);
    return () => clearInterval(timer);
  }, [working, reload]);

  const send = async (command: ExtensionCommand) => {
    try {
      list.set(await client.extensionCommand(command));
    } catch (error) {
      toast.error(error);
    }
  };

  const close = <Button icon="close" kind="ghost" accessibilityLabel="Close" onPress={() => router.back()} />;
  if (!list.data) return <Screen title="Extensions" right={close}>{list.error ? <Empty title={list.error} /> : <Spinner />}</Screen>;
  const data = list.data;

  return (
    <Screen title="Extensions" right={close}>
      {data.status && (
        <Card style={{ padding: 12 }}>
          <T size={12.5}>{data.status}</T>
        </Card>
      )}
      <Section>Installed</Section>
      {data.installed.length === 0 && <Empty icon="plugin" title="No extensions installed" />}
      {data.installed.map((e) => {
        const [label, tone] = STATE[e.state];
        const expanded = open === e.key;
        return (
          <Rows key={e.key}>
            <Row
              icon="plugin"
              title={e.name}
              detail={`${e.version}${e.update ? ` · ${e.update} is out` : ""} · ${e.source}`}
              right={<Pill label={label} tone={tone} />}
              chevron
              onPress={() => setOpen(expanded ? null : e.key)}
            />
            {expanded && (
              <View style={{ padding: 14, gap: 10 }}>
                <T size={12.5} tone="muted">{e.description}</T>
                {e.fault && <T size={12.5} tone="danger">{e.fault}</T>}
                {e.status.map((s) => <T key={s} size={12.5}>{s}</T>)}
                {e.duplicates.length > 0 && <T size={12} tone="warning">{`Also on from: ${e.duplicates.join(", ")}`}</T>}
                <View style={{ flexDirection: "row", alignItems: "center" }}>
                  <T size={14} weight="semibold" style={{ flex: 1 }}>On</T>
                  <Toggle value={e.enabled} disabled={data.busy} onChange={(on) => void send({ op: on ? "enable" : "disable", key: e.key })} />
                </View>
                {e.settings.map((s) => (
                  <View key={s.key} style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
                    <T size={14} style={{ flex: 1 }}>{s.label}</T>
                    {s.kind === "toggle" ? (
                      <Toggle value={s.value} onChange={(value) => void send({ op: "setting", key: e.key, setting: s.key, value })} />
                    ) : s.kind === "number" ? (
                      <>
                        <Button icon="minus" accessibilityLabel={`Lower ${s.label}`} disabled={s.value <= s.min} onPress={() => void send({ op: "setting", key: e.key, setting: s.key, value: s.value - 1 })} />
                        <T size={13} mono style={{ minWidth: 36, textAlign: "center" }}>{String(s.value)}</T>
                        <Button icon="plus" accessibilityLabel={`Raise ${s.label}`} disabled={s.value >= s.max} onPress={() => void send({ op: "setting", key: e.key, setting: s.key, value: s.value + 1 })} />
                      </>
                    ) : (
                      <Button label={s.options[s.value] ?? "—"} icon="swap" onPress={() => void send({ op: "setting", key: e.key, setting: s.key, value: (s.value + 1) % s.options.length })} />
                    )}
                  </View>
                ))}
                <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
                  {e.commands.map((c) => <Button key={c.id} label={c.label} onPress={() => void send({ op: "run", key: e.key, command: c.id })} />)}
                  {e.update && <Button label={`Update to ${e.update}`} kind="accent" disabled={data.busy} onPress={() => void send({ op: "update", key: e.key })} />}
                  {e.state === "faulted" && <Button label="Retry" onPress={() => void send({ op: "retry", key: e.key })} />}
                  <Button label="Remove" icon="trash" kind="ghost" onPress={() => ask(`Remove ${e.name}?`, "Its settings and stored data stay on the unit.", "Remove", () => void send({ op: "remove", key: e.key }))} />
                </View>
                {e.log.length > 0 && <T size={11} mono tone="faint">{e.log.slice(-5).join("\n")}</T>}
              </View>
            )}
          </Rows>
        );
      })}
      <View style={{ flexDirection: "row", alignItems: "center" }}>
        <Section>Available</Section>
        <View style={{ flex: 1 }} />
        <Button label="Refresh" icon="refresh" kind="ghost" disabled={data.busy} onPress={() => void send({ op: "refresh" })} />
      </View>
      {data.available.length === 0 ? (
        <Empty title="Everything in the catalog is installed" />
      ) : (
        <Rows>
          {data.available.map((a) => (
            <Row
              key={`${a.source}:${a.id}`}
              icon="plugin"
              title={a.name}
              detail={`${a.version} · ${a.description}`}
              right={<Button label="Install" kind="accent" disabled={data.busy} onPress={() => (a.app ? ask(`Install ${a.name}?`, "It brings a program with full access to the unit while it runs.", "Install", () => void send({ op: "install", source: a.source, id: a.id })) : void send({ op: "install", source: a.source, id: a.id }))} />}
            />
          ))}
        </Rows>
      )}
      <T size={11.5} tone="faint" center>
        Repositories are added and trusted on the unit only.
      </T>
    </Screen>
  );
}
