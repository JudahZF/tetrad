/** Connect: units nearby over Bluetooth, saved units, a Wi-Fi address, and the demo unit. */

import { fetch as expoFetch } from "expo/fetch";
import { Redirect, router } from "expo-router";
import { useCallback, useEffect, useState } from "react";
import { TextInput, View } from "react-native";
import { type FoundUnit, bluetoothAvailable, openLink, scanUnits } from "@/lib/link/ble";
import { UnitError } from "@/lib/unit/client";
import { DemoClient } from "@/lib/unit/demo";
import { LinkClient } from "@/lib/unit/link";
import { type Fetch, RemoteClient, parseAddress } from "@/lib/unit/remote";
import { type SavedUnit, forgetUnit, rememberUnit, savedUnits } from "@/state/storage";
import { errorText, useToast } from "@/state/toast";
import { useUnit } from "@/state/unit";
import { Logo } from "@/ui/Logo";
import { Button, Card, IconBox, Pill, Row, Rows, Screen, Section, T } from "@/ui/kit";
import { PinPad } from "@/ui/PinPad";
import { usePalette } from "@/ui/theme";

/** expo/fetch sends typed-array bodies; it wants them on their own ArrayBuffer. */
const fetchUnit: Fetch = (url, { body, ...init }) =>
  expoFetch(url, { ...init, ...(body !== undefined ? { body: typeof body === "string" ? body : new Uint8Array(body) } : {}) });

type Asking =
  | { kind: "bluetooth"; client: LinkClient; id: string }
  | { kind: "wifi"; host: string };

export default function Connect() {
  const { client, connect } = useUnit();
  const palette = usePalette();
  const toast = useToast();
  const [nearby, setNearby] = useState<FoundUnit[]>([]);
  const [scanNote, setScanNote] = useState<string | null>(null);
  const [saved, setSaved] = useState<SavedUnit[]>([]);
  const [address, setAddress] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [asking, setAsking] = useState<Asking | null>(null);
  const [pinError, setPinError] = useState<string | null>(null);

  useEffect(() => {
    void savedUnits().then(setSaved);
    let stop = () => {};
    void scanUnits(
      (unit) => setNearby((list) => (list.some((u) => u.id === unit.id) ? list.map((u) => (u.id === unit.id ? unit : u)) : [...list, unit])),
      setScanNote,
    ).then((s) => (stop = s));
    return () => stop();
  }, []);

  const enter = useCallback(
    async (next: LinkClient | RemoteClient | DemoClient, remember?: SavedUnit) => {
      if (remember) {
        await rememberUnit(remember);
        setSaved(await savedUnits());
      }
      setAsking(null);
      connect(next);
      router.replace("/rig");
    },
    [connect],
  );

  const openBluetooth = async (id: string, name: string) => {
    setBusy(id);
    try {
      const link = await LinkClient.open(await openLink(id));
      if (link.authorized) await enter(link, { kind: "bluetooth", id, name: link.name });
      else {
        setPinError(null);
        setAsking({ kind: "bluetooth", client: link, id });
      }
    } catch (error) {
      toast.error(error);
    } finally {
      setBusy(null);
    }
  };

  const openWifi = async (host: string, pin: string | null) => {
    if (!pin) {
      setPinError(null);
      setAsking({ kind: "wifi", host });
      return;
    }
    setBusy(host);
    try {
      const remote = await RemoteClient.connect(host, pin, fetchUnit);
      await enter(remote, { kind: "wifi", host, pin, name: host });
    } catch (error) {
      if (error instanceof UnitError && error.pin) {
        setPinError(null);
        setAsking({ kind: "wifi", host });
      } else toast.error(error);
    } finally {
      setBusy(null);
    }
  };

  const givePin = async (pin: string) => {
    if (!asking) return;
    setBusy("pin");
    setPinError(null);
    try {
      if (asking.kind === "bluetooth") {
        await asking.client.auth(pin);
        await enter(asking.client, { kind: "bluetooth", id: asking.id, name: asking.client.name });
      } else {
        const remote = await RemoteClient.connect(asking.host, pin, fetchUnit);
        await enter(remote, { kind: "wifi", host: asking.host, pin, name: asking.host });
      }
    } catch (error) {
      setPinError(errorText(error));
    } finally {
      setBusy(null);
    }
  };

  const cancelPin = () => {
    if (asking?.kind === "bluetooth") void asking.client.close();
    setAsking(null);
  };

  const addWifi = () => {
    const parsed = parseAddress(address);
    if (!parsed) return toast.error("Type the IP address that the unit shows, for example 192.168.1.20");
    void openWifi(parsed.host, parsed.pin);
  };

  if (client) return <Redirect href="/rig" />;

  const savedFor = (id: string) => saved.find((u) => u.kind === "bluetooth" && u.id === id);
  const nearbyIds = new Set(nearby.map((u) => u.id));

  return (
    <Screen>
      <View style={{ alignItems: "center", gap: 8, marginTop: 28, marginBottom: 12 }}>
        <Logo size={64} ring={palette.accent} />
        <T size={26} weight="bold">
          tetrad
        </T>
      </View>

      <Section>Nearby units · Bluetooth (experimental)</Section>
      {nearby.length > 0 ? (
        <Rows>
          {nearby.map((unit) => (
            <Row
              key={unit.id}
              icon="bluetooth"
              title={unit.name}
              detail={<T size={11} mono tone="muted">{`Bluetooth${unit.rssi !== null ? ` · ${unit.rssi} dBm` : ""}${savedFor(unit.id) ? " · paired" : " · new"}`}</T>}
              right={busy === unit.id ? <Pill label="Connecting…" tone="wait" /> : <Pill label="Connect" tone="ok" />}
              onPress={() => void openBluetooth(unit.id, unit.name)}
            />
          ))}
        </Rows>
      ) : (
        <Card style={{ padding: 14 }}>
          <T size={13} tone="muted">
            {scanNote ??
              (bluetoothAvailable
                ? "Looking for units… Turn on Bluetooth in Settings › Tetrad › Bluetooth on the unit. Bluetooth is experimental: it needs a Tetrad with the Tetrad Link, and it is not tested on a unit yet. Wi-Fi is the tested way."
                : "Bluetooth works in the phone app.")}
          </T>
        </Card>
      )}

      {saved.length > 0 && (
        <>
          <Section>Saved units</Section>
          <Rows>
            {saved
              .filter((u) => u.kind === "wifi" || !nearbyIds.has(u.id))
              .map((unit) => (
                <Row
                  key={unit.kind === "wifi" ? unit.host : unit.id}
                  icon={unit.kind === "wifi" ? "wifi" : "bluetooth"}
                  title={unit.name}
                  detail={<T size={11} mono tone="muted">{unit.kind === "wifi" ? `Wi-Fi Remote · ${unit.host}` : "Bluetooth · not in range"}</T>}
                  right={
                    <Button
                      icon="trash"
                      kind="ghost"
                      style={{ minHeight: 32, paddingHorizontal: 8 }}
                      onPress={() => void forgetUnit(unit).then(savedUnits).then(setSaved)}
                    />
                  }
                  chevron
                  onPress={() => (unit.kind === "wifi" ? void openWifi(unit.host, unit.pin) : void openBluetooth(unit.id, unit.name))}
                />
              ))}
          </Rows>
        </>
      )}

      <Section>Wi-Fi</Section>
      <Card style={{ padding: 14, gap: 10 }}>
        <T size={12.5} tone="muted">
          Turn on Remote in Settings › Tetrad › Remote. Type the address it shows, or paste the link of its QR code.
        </T>
        <View style={{ flexDirection: "row", gap: 8, alignItems: "center" }}>
          <IconBox name="wifi" />
          <TextInput
            value={address}
            onChangeText={setAddress}
            onSubmitEditing={addWifi}
            placeholder="192.168.1.20"
            placeholderTextColor={palette.faint}
            keyboardType="url"
            autoCapitalize="none"
            autoCorrect={false}
            accessibilityLabel="Unit address"
            style={{ flex: 1, color: palette.text, backgroundColor: palette.canvas, borderRadius: 8, paddingHorizontal: 12, height: 40, fontFamily: "IBMPlexMono_500Medium" }}
          />
          <Button label="Connect" kind="accent" busy={busy !== null && busy === parseAddress(address)?.host} onPress={addWifi} />
        </View>
      </Card>

      <Button label="Try the demo unit" kind="panel" style={{ marginTop: 8 }} onPress={() => void enter(new DemoClient())} />
      <T size={11.5} tone="faint" center>
        The demo has every feature, including Grid editing, with no Quad Cortex.
      </T>

      <PinPad
        visible={asking !== null}
        title={asking?.kind === "wifi" ? "Enter the Remote PIN" : "Enter the App PIN"}
        hint={
          asking?.kind === "wifi"
            ? "Settings › Tetrad › Remote on the unit shows it."
            : "Settings › Tetrad › Bluetooth on the unit shows it. The unit remembers this phone after one PIN."
        }
        busy={busy === "pin"}
        error={pinError}
        onPin={(pin) => void givePin(pin)}
        onCancel={cancelPin}
      />
    </Screen>
  );
}
