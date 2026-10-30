/** Unit: the connected unit, its theme, and the pages of backup, extensions, notices and the support file. */

import { router, useFocusEffect } from "expo-router";
import { useCallback, useState } from "react";
import { Pressable, View } from "react-native";
import { BUILTIN_THEMES } from "@/lib/themes";
import type { ThemeEntry } from "@/lib/unit/types";
import { shareFile } from "@/state/files";
import { useToast } from "@/state/toast";
import { useClient, useUnit } from "@/state/unit";
import { useLoad } from "@/state/useLoad";
import { Icon } from "@/ui/Icon";
import { Button, Card, Empty, Pill, Row, Rows, Screen, Section, Spinner, T } from "@/ui/kit";
import { usePalette } from "@/ui/theme";

const TRANSPORT = { bluetooth: "Bluetooth (experimental)", wifi: "Wi-Fi Remote", demo: "Demo" } as const;

function Swatch({ theme, on, onPress }: { theme: ThemeEntry; on: boolean; onPress: () => void }) {
  const palette = usePalette();
  const colours = theme.palette ?? BUILTIN_THEMES.find((t) => t.id === theme.id)?.palette;
  return (
    <Pressable onPress={onPress} disabled={!!theme.error} accessibilityRole="button" accessibilityState={{ selected: on }} accessibilityLabel={theme.name} style={{ width: 76, gap: 4, opacity: theme.error ? 0.4 : 1 }}>
      <View style={{ height: 46, borderRadius: 8, backgroundColor: colours?.background ?? palette.surface, borderWidth: 2, borderColor: on ? palette.accent : "#ffffff14", overflow: "hidden" }}>
        <View style={{ position: "absolute", left: 6, top: 6, width: 24, height: 12, borderRadius: 3, backgroundColor: colours?.panel ?? palette.panel }} />
        <View style={{ position: "absolute", left: 6, right: 6, bottom: 6, height: 6, borderRadius: 3, backgroundColor: colours?.accent ?? palette.accent }} />
      </View>
      <T size={10.5} center tone={on ? "accent" : "muted"} lines={1}>
        {theme.name}
      </T>
    </Pressable>
  );
}

export default function Unit() {
  const client = useClient();
  const { disconnect, followTheme } = useUnit();
  const palette = usePalette();
  const toast = useToast();
  const themes = useLoad(() => client.themes(), client);
  const tabs = useLoad(() => client.tabs(), client);
  // Extensions can gain or lose a page on the Extensions screen: ask again on each return here.
  const { reload: reloadTabs } = tabs;
  useFocusEffect(
    useCallback(() => {
      void reloadTabs();
    }, [reloadTabs]),
  );
  const [sending, setSending] = useState(false);

  const selectTheme = async (id: string) => {
    try {
      const next = await client.themeSelect(id);
      themes.set(next);
      followTheme(next);
    } catch (error) {
      toast.error(error);
    }
  };

  const report = async () => {
    setSending(true);
    try {
      const file = await client.support();
      await shareFile(file.name, file.zip, "application/zip");
    } catch (error) {
      toast.error(error);
    } finally {
      setSending(false);
    }
  };

  return (
    <Screen title="Unit">
      <Card style={{ padding: 14, flexDirection: "row", gap: 12, alignItems: "center" }}>
        <View style={{ width: 46, height: 46, borderRadius: 10, backgroundColor: palette.canvas, borderWidth: 2, borderColor: palette.accent, alignItems: "center", justifyContent: "center" }}>
          <Icon name="device" size={28} color={palette.accent} />
        </View>
        <View style={{ flex: 1, minWidth: 0 }}>
          <T size={15} weight="bold" lines={1}>
            {client.name}
          </T>
          <T size={11} mono tone="muted" lines={1}>
            {[client.version && `Tetrad ${client.version}`, TRANSPORT[client.transport]].filter(Boolean).join(" · ")}
          </T>
        </View>
        <Pill label="Connected" tone="ok" />
      </Card>

      <Section>Theme</Section>
      <Card style={{ padding: 12 }}>
        {themes.data ? (
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 10 }}>
            {themes.data.themes.map((t) => (
              <Swatch key={t.id} theme={t} on={t.id === themes.data?.active} onPress={() => void selectTheme(t.id)} />
            ))}
          </View>
        ) : themes.error ? (
          <T tone="danger">{themes.error}</T>
        ) : (
          <Spinner />
        )}
      </Card>
      <T size={11.5} tone="faint">The unit changes to the theme at once, and the app follows it.</T>

      <Rows>
        <Row icon="archive" title="Backup" detail="Back up the unit to the phone, or send a backup back" chevron onPress={() => router.push("/backup")} />
        <Row icon="plugin" title="Extensions" detail="Install, update, turn on and off" chevron onPress={() => router.push("/extensions")} />
        <Row icon="info" title="Notices" detail="What the unit reported lately" chevron onPress={() => router.push("/notices")} />
        <Row icon="bluetooth" title="Bluetooth" detail="Host mode and MIDI over Bluetooth are set on the unit: Settings › Tetrad › Bluetooth" />
        <Row icon="envelope" title="Send Report" detail="The support file, for a bug report" right={sending ? <Spinner /> : undefined} disabled={sending} onPress={() => void report()} />
      </Rows>

      <Section>Extension pages</Section>
      {tabs.data && tabs.data.length > 0 ? (
        <Rows>
          {tabs.data.map((tab) => (
            <Row key={tab.id} icon="plugin" title={tab.title} chevron onPress={() => router.push({ pathname: "/extension/[id]", params: { id: tab.id, title: tab.title } })} />
          ))}
        </Rows>
      ) : tabs.loading ? (
        <Spinner />
      ) : (
        <Empty title="No extension has a page" />
      )}

      <Button
        label="Disconnect"
        icon="power"
        kind="ghost"
        onPress={() => {
          void disconnect();
          router.replace("/");
        }}
      />
    </Screen>
  );
}
