/** Notices: what the unit reported lately, newest first. */

import { router } from "expo-router";
import { useClient } from "@/state/unit";
import { useLoad } from "@/state/useLoad";
import { Button, Empty, Pill, Row, Rows, Screen, Spinner, T } from "@/ui/kit";

const ago = (seconds: number) =>
  seconds < 60 ? `${Math.round(seconds)} s ago` : seconds < 3600 ? `${Math.round(seconds / 60)} min ago` : `${Math.round(seconds / 3600)} h ago`;

export default function Notices() {
  const client = useClient();
  const notices = useLoad(() => client.notices(), client, undefined, 8000);
  const list = [...(notices.data?.notices ?? [])].reverse();
  return (
    <Screen title="Notices" right={<Button icon="close" kind="ghost" accessibilityLabel="Close" onPress={() => router.back()} />}>
      {!notices.data ? (
        notices.error ? <Empty title={notices.error} /> : <Spinner />
      ) : list.length === 0 ? (
        <Empty icon="info" title="No notices" />
      ) : (
        <Rows>
          {list.map((n, i) => (
            <Row
              key={i}
              title={<T size={13}>{n.detail}</T>}
              detail={<T size={11} mono tone="muted">{ago(n.ago)}</T>}
              right={<Pill label={n.subsystem} tone="plain" />}
            />
          ))}
        </Rows>
      )}
      {notices.data && notices.data.dropped > 0 && (
        <T size={11.5} tone="faint" center>{`${notices.data.dropped} older notices are no longer kept.`}</T>
      )}
    </Screen>
  );
}
