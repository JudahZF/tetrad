/**
 * One interface for every way to reach a unit: the Tetrad Link (Bluetooth), Remote (Wi-Fi) and
 * the demo unit. Screens check `has` for the features that only some transports have, and say
 * what a transport lacks instead of failing.
 */

import type { BlockModel } from "@/lib/preset/edit";
import type { Edit, Preset } from "@/lib/preset/model";
import type {
  BackupStatus,
  ExtensionCommand,
  ExtensionTab,
  ExtensionsList,
  FileChunkReply,
  FilesReply,
  FootswitchCommand,
  FootswitchView,
  IrAddedReply,
  IrLibrary,
  LibraryOp,
  MappingCommand,
  MappingsView,
  NoticesReply,
  StatusReply,
  SupportFile,
  ThemesList,
} from "./types";

export type Transport = "bluetooth" | "wifi" | "demo";

/**
 * What only some transports have. Every client has the whole Remote API: Wi-Fi as HTTP, the
 * Link through its `remote` op, and the demo unit in the app.
 */
export type Feature =
  /** The live preset and its edits (docs/preset-protocol.md). */
  | "preset"
  /** Messages to extensions with the `ble` capability. */
  | "extension-messages";

export const FEATURE_NAMES: Record<Feature, string> = {
  preset: "preset editing",
  "extension-messages": "extension messages",
};

export class UnitError extends Error {
  constructor(
    message: string,
    /** The unit refused (as opposed to a lost connection). Show the text as it is. */
    readonly refused = true,
    /** The PIN is no longer good: ask for it again. */
    readonly pin = false,
    /** The HTTP status of a Remote refusal. */
    readonly status?: number,
  ) {
    super(message);
  }
}

export class Unsupported extends UnitError {
  constructor(feature: Feature, transport: Transport) {
    super(`${FEATURE_NAMES[feature]} is not available over ${transport === "wifi" ? "Wi-Fi" : "Bluetooth"}`, true);
  }
}

export type UnitEvent =
  | { kind: "preset"; preset: Preset }
  | { kind: "extension"; extension: string; value: unknown }
  | { kind: "closed"; reason: string };

export interface UnitClient {
  readonly transport: Transport;
  /** The unit's name ("Quad Cortex 1FEB") or its address. */
  readonly name: string;
  /** Tetrad's version, when the transport reports it. */
  readonly version: string | null;
  has(feature: Feature): boolean;

  status(): Promise<StatusReply>;
  uploadIr(name: string, wav: Uint8Array): Promise<IrAddedReply>;
  library(): Promise<IrLibrary>;
  libraryOp(op: LibraryOp): Promise<IrLibrary>;
  files(): Promise<FilesReply>;
  fileChunk(extension: string, name: string, offset: number, last: boolean, bytes: Uint8Array): Promise<FileChunkReply>;
  fileDelete(extension: string, name: string): Promise<void>;
  tabs(): Promise<ExtensionTab[]>;
  tabPage(extension: string): Promise<string>;
  tabCall(extension: string, message: unknown): Promise<unknown>;
  support(): Promise<SupportFile>;
  notices(): Promise<NoticesReply>;
  backup(): Promise<BackupStatus>;
  backupCreate(): Promise<BackupStatus>;
  backupPiece(offset: number): Promise<Uint8Array>;
  backupUpload(name: string, offset: number, last: boolean, bytes: Uint8Array): Promise<BackupStatus>;
  backupDiscard(): Promise<BackupStatus>;
  extensions(): Promise<ExtensionsList>;
  extensionCommand(command: ExtensionCommand): Promise<ExtensionsList>;
  extensionMessage(extension: string, message: unknown): Promise<unknown>;
  themes(): Promise<ThemesList>;
  themeSelect(id: string): Promise<ThemesList>;
  mappings(): Promise<MappingsView>;
  mappingCommand(command: MappingCommand): Promise<MappingsView>;
  footswitch(): Promise<FootswitchView>;
  footswitchCommand(command: FootswitchCommand): Promise<FootswitchView>;
  preset(): Promise<Preset>;
  /** Refused when `loadId` is not the preset's now (someone else edited it first). */
  presetEdit(loadId: number, edit: Edit): Promise<Preset>;
  presetSave(): Promise<Preset>;
  blocks(): Promise<BlockModel[]>;

  onEvent(listener: (event: UnitEvent) => void): () => void;
  close(): Promise<void>;
}

/** Up to 48 KiB per piece: the unit takes bodies of at most 64 KiB. */
export const FILE_PIECE = 48 * 1024;

/** Sends a file in pieces; `progress` gets the bytes sent. */
export async function sendFile(
  client: UnitClient,
  extension: string,
  name: string,
  bytes: Uint8Array,
  progress: (sent: number) => void = () => {},
): Promise<FileChunkReply> {
  let offset = 0;
  for (;;) {
    const piece = bytes.subarray(offset, offset + FILE_PIECE);
    const last = offset + piece.length >= bytes.length;
    const reply = await client.fileChunk(extension, name, offset, last, piece);
    offset += piece.length;
    progress(offset);
    if (last) return reply;
  }
}
