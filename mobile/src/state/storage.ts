/**
 * The units the app knows: Wi-Fi units with their Remote PIN, Bluetooth units by their radio id.
 * Kept in the phone's secure store (the PIN is a secret); on the web, in local storage.
 */

import { Platform } from "react-native";
import * as SecureStore from "expo-secure-store";

export type SavedUnit =
  | { kind: "wifi"; host: string; pin: string; name: string }
  | { kind: "bluetooth"; id: string; name: string };

const KEY = "tetrad.units";

async function read(): Promise<string | null> {
  if (Platform.OS === "web") return globalThis.localStorage?.getItem(KEY) ?? null;
  return SecureStore.getItemAsync(KEY);
}

async function write(value: string) {
  if (Platform.OS === "web") globalThis.localStorage?.setItem(KEY, value);
  else await SecureStore.setItemAsync(KEY, value);
}

export async function savedUnits(): Promise<SavedUnit[]> {
  try {
    const parsed: unknown = JSON.parse((await read()) ?? "[]");
    return Array.isArray(parsed) ? (parsed as SavedUnit[]) : [];
  } catch {
    return [];
  }
}

const same = (a: SavedUnit, b: SavedUnit) =>
  a.kind === b.kind && (a.kind === "wifi" ? a.host === (b as typeof a).host : a.id === (b as typeof a).id);

/** Puts `unit` first, replacing an older entry for the same unit. */
export async function rememberUnit(unit: SavedUnit) {
  const units = (await savedUnits()).filter((u) => !same(u, unit));
  await write(JSON.stringify([unit, ...units].slice(0, 8)));
}

export async function forgetUnit(unit: SavedUnit) {
  await write(JSON.stringify((await savedUnits()).filter((u) => !same(u, unit))));
}
