/** Add block: the unit's block list with the Grid's glyphs and colours; extension blocks first. */

import { router, useLocalSearchParams } from "expo-router";
import { useMemo, useState } from "react";
import { Pressable, ScrollView, TextInput, View } from "react-native";
import type { BlockModel } from "@/lib/preset/edit";
import { CATEGORIES, type Category, blockAt, parseSlot } from "@/lib/preset/model";
import { useUnit } from "@/state/unit";
import { BlockTile } from "@/ui/BlockTile";
import { Icon } from "@/ui/Icon";
import { Button, Empty, Pill, Row, Rows, Screen, Section, Spinner, T } from "@/ui/kit";
import { usePalette } from "@/ui/theme";

export default function AddBlock() {
  const params = useLocalSearchParams<{ slot: string }>();
  const slot = parseSlot(params.slot);
  const { preset, blocks, edit } = useUnit();
  const palette = usePalette();
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState<Category | null>(null);
  const replacing = preset && slot ? blockAt(preset, slot) : null;

  const categories = useMemo(() => [...new Set(blocks.map((b) => b.category))], [blocks]);
  const groups = useMemo(() => {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    const match = (b: BlockModel) =>
      (!category || b.category === category) &&
      words.every((w) => `${b.name} ${b.description ?? ""} ${b.extension ?? ""}`.toLowerCase().includes(w));
    const found = blocks.filter(match);
    const byGroup = new Map<string, BlockModel[]>();
    for (const b of found) {
      const group = b.extension ? `${b.extension} · extension` : CATEGORIES[b.category].name;
      byGroup.set(group, [...(byGroup.get(group) ?? []), b]);
    }
    // Extension blocks first: they are the ones the unit's own picker shows last.
    return [...byGroup].sort(([a], [b]) => Number(b.endsWith("extension")) - Number(a.endsWith("extension")));
  }, [blocks, query, category]);

  if (!slot) return null;

  const add = (model: BlockModel) => {
    edit({ kind: "add", to: slot, model: model.model });
    router.back();
  };

  return (
    <Screen
      title={replacing ? `Replace ${replacing.name}` : "Add block"}
      right={
        <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <T size={11} mono tone="muted">{`Row ${slot.row + 1} · Col ${slot.column + 1}`}</T>
          <Button icon="close" kind="ghost" onPress={() => router.back()} />
        </View>
      }
    >
      <View style={{ flexDirection: "row", alignItems: "center", gap: 8, backgroundColor: palette.surface, borderRadius: 10, paddingHorizontal: 12 }}>
        <Icon name="search" size={18} color={palette.faint} />
        <TextInput
          value={query}
          onChangeText={setQuery}
          placeholder="Search blocks"
          placeholderTextColor={palette.faint}
          autoCorrect={false}
          style={{ flex: 1, height: 40, color: palette.text, fontFamily: "IBMPlexSans_400Regular" }}
          accessibilityLabel="Search blocks"
        />
      </View>
      <ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={{ gap: 6 }}>
        {categories.map((c) => {
          const on = c === category;
          return (
            <Pressable
              key={c}
              onPress={() => setCategory(on ? null : c)}
              accessibilityRole="button"
              accessibilityState={{ selected: on }}
              style={{
                flexDirection: "row",
                alignItems: "center",
                gap: 6,
                paddingHorizontal: 10,
                paddingVertical: 6,
                borderRadius: 14,
                backgroundColor: on ? palette.accentFill : palette.surface,
                borderWidth: 1,
                borderColor: on ? palette.accent : "transparent",
              }}
            >
              <View style={{ width: 7, height: 7, borderRadius: 4, backgroundColor: CATEGORIES[c].colour }} />
              <T size={12} weight="semibold" tone={on ? "accent" : "muted"}>
                {CATEGORIES[c].name}
              </T>
            </Pressable>
          );
        })}
      </ScrollView>
      {blocks.length === 0 ? (
        <Spinner />
      ) : groups.length === 0 ? (
        <Empty icon="search" title="No block matches" />
      ) : (
        groups.map(([group, models]) => (
          <View key={group} style={{ gap: 12 }}>
            <Section>{group}</Section>
            <Rows>
              {models.map((m) => (
                <Row
                  key={m.model}
                  lead={<BlockTile category={m.category} size={34} />}
                  title={m.name}
                  detail={m.description ?? CATEGORIES[m.category].name}
                  right={replacing?.model === m.model ? <Pill label="In use" tone="ok" /> : undefined}
                  onPress={() => add(m)}
                />
              ))}
            </Rows>
          </View>
        ))
      )}
    </Screen>
  );
}
