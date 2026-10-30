/** Files from the phone (the system picker) and files to the phone (the share sheet). */

import * as DocumentPicker from "expo-document-picker";
import { File, Paths } from "expo-file-system";
import * as Sharing from "expo-sharing";
import { Platform } from "react-native";

export interface PickedFile {
  name: string;
  size: number;
  bytes: () => Promise<Uint8Array>;
}

/** Lets the user choose files; `types` are MIME types or "*\/*". */
export async function pickFiles(types: string[]): Promise<PickedFile[]> {
  const result = await DocumentPicker.getDocumentAsync({ type: types, multiple: true, copyToCacheDirectory: true });
  if (result.canceled) return [];
  return result.assets.map((asset) => ({
    name: asset.name,
    size: asset.size ?? 0,
    bytes: async () =>
      Platform.OS === "web" && asset.file
        ? new Uint8Array(await asset.file.arrayBuffer())
        : new File(asset.uri).bytes(),
  }));
}

/** Hands a file from the unit to the phone: the share sheet, or a download on the web. */
export async function shareFile(name: string, bytes: Uint8Array, mimeType: string) {
  if (Platform.OS === "web") {
    const url = URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: mimeType }));
    const link = document.createElement("a");
    link.href = url;
    link.download = name;
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
    return;
  }
  // The name comes from the unit: keep only its last part, so it stays in the cache.
  const base = name.split(/[/\\]/).pop() ?? "";
  const file = new File(Paths.cache, base && base !== "." && base !== ".." ? base : "tetrad-file");
  if (file.exists) file.delete();
  file.create();
  await file.write(bytes);
  await Sharing.shareAsync(file.uri, { mimeType, dialogTitle: name });
}

export const formatBytes = (n: number) =>
  n >= 1024 * 1024 ? `${(n / 1024 / 1024).toFixed(1)} MB` : n >= 1024 ? `${Math.round(n / 1024)} KB` : `${n} B`;
