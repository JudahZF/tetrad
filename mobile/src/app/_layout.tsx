import { IBMPlexMono_500Medium } from "@expo-google-fonts/ibm-plex-mono";
import {
  IBMPlexSans_400Regular,
  IBMPlexSans_500Medium,
  IBMPlexSans_600SemiBold,
  IBMPlexSans_700Bold,
} from "@expo-google-fonts/ibm-plex-sans";
import { useFonts } from "expo-font";
import { Stack } from "expo-router";
import * as SplashScreen from "expo-splash-screen";
import { StatusBar } from "expo-status-bar";
import { useEffect } from "react";
import { SafeAreaProvider } from "react-native-safe-area-context";
import { ToastProvider } from "@/state/toast";
import { UnitProvider, useUnit } from "@/state/unit";
import { ThemeProvider, usePalette } from "@/ui/theme";

void SplashScreen.preventAutoHideAsync();

function Navigator() {
  const palette = usePalette();
  const { client } = useUnit();
  const sheet = { presentation: "modal" as const, contentStyle: { backgroundColor: palette.background } };
  return (
    <>
      <StatusBar style="light" />
      <Stack screenOptions={{ headerShown: false, contentStyle: { backgroundColor: palette.background } }}>
        <Stack.Screen name="index" />
        {/* A lost link closes every screen that needs the unit (useClient). */}
        <Stack.Protected guard={client !== null}>
          <Stack.Screen name="(tabs)" />
          <Stack.Screen name="block/[slot]" options={sheet} />
          <Stack.Screen name="add/[slot]" options={sheet} />
          <Stack.Screen name="mapping" options={sheet} />
          <Stack.Screen name="switch-action" options={sheet} />
          <Stack.Screen name="extension/[id]" options={sheet} />
          <Stack.Screen name="extensions" options={sheet} />
          <Stack.Screen name="backup" options={sheet} />
          <Stack.Screen name="notices" options={sheet} />
        </Stack.Protected>
      </Stack>
    </>
  );
}

export default function RootLayout() {
  const [loaded] = useFonts({
    IBMPlexSans_400Regular,
    IBMPlexSans_500Medium,
    IBMPlexSans_600SemiBold,
    IBMPlexSans_700Bold,
    IBMPlexMono_500Medium,
  });
  useEffect(() => {
    if (loaded) void SplashScreen.hideAsync();
  }, [loaded]);
  if (!loaded) return null;
  return (
    <ThemeProvider>
      <SafeAreaProvider>
        <ToastProvider>
          <UnitProvider>
            <Navigator />
          </UnitProvider>
        </ToastProvider>
      </SafeAreaProvider>
    </ThemeProvider>
  );
}
