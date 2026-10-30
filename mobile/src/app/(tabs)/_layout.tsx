import { Redirect, Tabs } from "expo-router";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { useUnit } from "@/state/unit";
import { Icon, type IconName } from "@/ui/Icon";
import { T } from "@/ui/kit";
import { usePalette } from "@/ui/theme";

const TABS: { name: string; title: string; icon: IconName }[] = [
  { name: "rig", title: "Rig", icon: "grid" },
  { name: "library", title: "Library", icon: "folder" },
  { name: "control", title: "Control", icon: "footswitch" },
  { name: "unit", title: "Unit", icon: "chip" },
];

export default function TabsLayout() {
  const { client } = useUnit();
  const palette = usePalette();
  const insets = useSafeAreaInsets();
  if (!client) return <Redirect href="/" />;
  return (
    <Tabs
      screenOptions={{
        headerShown: false,
        tabBarActiveTintColor: palette.accent,
        tabBarInactiveTintColor: palette.faint,
        tabBarStyle: {
          backgroundColor: palette.canvas,
          borderTopColor: palette.surface,
          height: 60 + insets.bottom,
          paddingTop: 6,
          paddingBottom: insets.bottom + 6,
        },
        sceneStyle: { backgroundColor: palette.background },
      }}
    >
      {TABS.map((tab) => (
        <Tabs.Screen
          key={tab.name}
          name={tab.name}
          options={{
            title: tab.title,
            tabBarIcon: ({ color }) => <Icon name={tab.icon} size={24} color={color} />,
            tabBarLabel: ({ color }) => (
              <T size={10.5} weight="medium" color={String(color)} style={{ marginTop: 2 }}>
                {tab.title}
              </T>
            ),
          }}
        />
      ))}
    </Tabs>
  );
}
