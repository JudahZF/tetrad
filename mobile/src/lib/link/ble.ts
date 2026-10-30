/** Web and tests: no Bluetooth (ble.native.ts is the phone's). */

import type { LinkPort } from "./session";

export interface FoundUnit {
  id: string;
  name: string;
  rssi: number | null;
}

export const bluetoothAvailable = false;

export async function scanUnits(_onFound: (unit: FoundUnit) => void, onError: (message: string) => void): Promise<() => void> {
  onError("Bluetooth works in the phone app. Use Wi-Fi or the demo unit here.");
  return () => {};
}

export async function openLink(_id: string): Promise<LinkPort> {
  throw new Error("Bluetooth works in the phone app.");
}
