/**
 * The radio: finds units and opens a Tetrad Link port with react-native-ble-plx. The first
 * write to the Link starts the pairing (the characteristics need an encrypted link); the
 * phone shows its own pairing prompt.
 */

import { PermissionsAndroid, Platform } from "react-native";
import { BleManager, type Device, type Subscription } from "react-native-ble-plx";
import { fromBase64, toBase64 } from "./base64";
import type { LinkPort } from "./session";
import { LINK_REPLY, LINK_REQUEST, LINK_SERVICE, MIDI_SERVICE, MTU } from "./uuids";

export interface FoundUnit {
  id: string;
  name: string;
  rssi: number | null;
}

let manager: BleManager | null = null;
const ble = () => (manager ??= new BleManager());

export const bluetoothAvailable = true;

async function permitted(): Promise<boolean> {
  if (Platform.OS !== "android") return true;
  const wanted =
    Number(Platform.Version) >= 31
      ? [PermissionsAndroid.PERMISSIONS.BLUETOOTH_SCAN, PermissionsAndroid.PERMISSIONS.BLUETOOTH_CONNECT]
      : [PermissionsAndroid.PERMISSIONS.ACCESS_FINE_LOCATION];
  const granted = await PermissionsAndroid.requestMultiple(wanted);
  return wanted.every((p) => granted[p] === PermissionsAndroid.RESULTS.GRANTED);
}

/** Scans for units until the returned stop runs. */
export async function scanUnits(onFound: (unit: FoundUnit) => void, onError: (message: string) => void): Promise<() => void> {
  if (!(await permitted())) {
    onError("Tetrad needs the Bluetooth permission to find your unit.");
    return () => {};
  }
  const state = await ble().state();
  if (state !== "PoweredOn") onError("Turn on Bluetooth to find your unit.");
  await ble().startDeviceScan([MIDI_SERVICE], { allowDuplicates: false }, (error, device) => {
    if (error) return onError(error.message);
    if (!device) return;
    onFound({ id: device.id, name: device.localName ?? device.name ?? "Quad Cortex", rssi: device.rssi });
  });
  return () => void ble().stopDeviceScan();
}

class BlePort implements LinkPort {
  constructor(
    private readonly device: Device,
    readonly frameSize: number,
  ) {}

  async write(frame: Uint8Array) {
    await this.device.writeCharacteristicWithoutResponseForService(LINK_SERVICE, LINK_REQUEST, toBase64(frame));
  }

  listen(onFrame: (frame: Uint8Array) => void, onError: (error: Error) => void) {
    const subscriptions: Subscription[] = [
      this.device.monitorCharacteristicForService(LINK_SERVICE, LINK_REPLY, (error, characteristic) => {
        if (error) return onError(new Error(error.message));
        if (characteristic?.value) onFrame(fromBase64(characteristic.value));
      }),
      this.device.onDisconnected(() => onError(new Error("The unit disconnected"))),
    ];
    return () => subscriptions.forEach((s) => s.remove());
  }

  async close() {
    await this.device.cancelConnection().catch(() => undefined);
  }
}

export async function openLink(id: string): Promise<LinkPort> {
  await ble().stopDeviceScan();
  const device = await ble().connectToDevice(id, { requestMTU: MTU, timeout: 15_000 });
  await device.discoverAllServicesAndCharacteristics();
  const services = await device.services();
  if (!services.some((s) => s.uuid.toLowerCase() === LINK_SERVICE)) {
    await device.cancelConnection();
    throw new Error("This unit has no Tetrad Link. Update Tetrad and turn on Bluetooth on the unit.");
  }
  // iOS picks the MTU itself; the device reports what it got.
  return new BlePort(device, Math.max((device.mtu || 23) - 3, 20));
}
