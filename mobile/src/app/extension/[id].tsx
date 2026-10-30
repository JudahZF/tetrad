/** An extension's own page (its Remote tab), with `tetrad.send` to its extension. */

import { router, useLocalSearchParams } from "expo-router";
import { useCallback } from "react";
import { View } from "react-native";
import { useClient } from "@/state/unit";
import { useLoad } from "@/state/useLoad";
import { Button, Empty, Screen, Spinner } from "@/ui/kit";
import { TabFrame } from "@/ui/TabFrame";

export default function ExtensionPage() {
  const { id, title } = useLocalSearchParams<{ id: string; title?: string }>();
  const client = useClient();
  const page = useLoad(() => client.tabPage(id), client, id);
  const call = useCallback((message: unknown) => client.tabCall(id, message), [client, id]);
  return (
    <Screen title={title ?? id} right={<Button icon="close" kind="ghost" accessibilityLabel="Close" onPress={() => router.back()} />} scroll={false}>
      {page.data ? (
        <View style={{ height: "100%", minHeight: 400, borderRadius: 10, overflow: "hidden" }}>
          <TabFrame html={page.data} call={call} />
        </View>
      ) : page.error ? (
        <Empty icon="plugin" title="The page did not load" detail={page.error} />
      ) : (
        <Spinner />
      )}
    </Screen>
  );
}
