/**
 * The connected unit: its client, the live preset (with the edit queue), the block list, and
 * the unit's theme as the app's palette.
 */

import { type ReactNode, createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { type BlockModel, applyEdit } from "@/lib/preset/edit";
import type { Edit, Preset } from "@/lib/preset/model";
import { EditQueue } from "@/lib/preset/queue";
import { BUILTIN_THEMES, STOCK } from "@/lib/themes";
import type { UnitClient } from "@/lib/unit/client";
import type { ThemesList } from "@/lib/unit/types";
import { useSetPalette } from "@/ui/theme";
import { useToast } from "./toast";

interface UnitState {
  client: UnitClient | null;
  /** null while it loads, or when the transport has no preset. */
  preset: Preset | null;
  blocks: BlockModel[];
  /** Edits wait to be sent or are on their way. */
  sending: boolean;
  connect(client: UnitClient): void;
  disconnect(): Promise<void>;
  edit(edit: Edit): void;
  save(): Promise<void>;
  reloadPreset(): Promise<void>;
  /** Uses the palette of the unit's active theme. */
  followTheme(themes: ThemesList): void;
}

const UnitContext = createContext<UnitState | null>(null);

export function UnitProvider({ children }: { children: ReactNode }) {
  const [client, setClient] = useState<UnitClient | null>(null);
  const [preset, setPreset] = useState<Preset | null>(null);
  const [blocks, setBlocks] = useState<BlockModel[]>([]);
  const [sending, setSending] = useState(false);
  const presetRef = useRef<Preset | null>(null);
  const setPalette = useSetPalette();
  const toast = useToast();

  // The catalog for edits made outside a render (a reply that arrives later).
  const catalog = useRef<BlockModel[]>([]);
  useEffect(() => {
    catalog.current = blocks;
  }, [blocks]);

  const showPreset = useCallback((next: Preset, waiting: readonly Edit[] = []) => {
    // An older preset (a late event) never replaces a newer one.
    if (presetRef.current && next.loadId < presetRef.current.loadId && next.name === presetRef.current.name) return;
    presetRef.current = next;
    // Edits that still wait stay on screen: the reply has only the edits sent before them.
    setPreset(
      waiting.reduce((shown, edit) => {
        try {
          return applyEdit(shown, edit, catalog.current);
        } catch {
          return shown;
        }
      }, next),
    );
  }, []);

  const followTheme = useCallback(
    (themes: ThemesList) => {
      const active = themes.themes.find((t) => t.id === themes.active);
      setPalette(active?.palette ?? BUILTIN_THEMES.find((t) => t.id === themes.active)?.palette ?? STOCK);
    },
    [setPalette],
  );

  const reloadPreset = useCallback(async () => {
    if (!client?.has("preset")) return;
    try {
      const fresh = await client.preset();
      presetRef.current = fresh;
      setPreset(fresh);
    } catch (error) {
      toast.error(error);
    }
  }, [client, toast]);

  // Made for each client in the effect below; used in handlers only.
  const queue = useRef<EditQueue | null>(null);

  useEffect(() => {
    if (!client) return;
    const edits = new EditQueue(
      () => presetRef.current,
      (loadId, edit) => client.presetEdit(loadId, edit),
      showPreset,
      (error) => {
        toast.error(error);
        void reloadPreset();
      },
      () => setSending(false),
    );
    queue.current = edits;
    const off = client.onEvent((event) => {
      if (event.kind === "preset") showPreset(event.preset);
      if (event.kind === "closed") {
        setClient(null);
        toast.error(event.reason);
      }
    });
    let live = true;
    if (client.has("preset")) {
      client.preset().then((fresh) => {
        if (!live) return;
        presetRef.current = fresh;
        setPreset(fresh);
      }, toast.error);
      client.blocks().then((list) => live && setBlocks(list), toast.error);
    }
    client.themes().then((themes) => live && followTheme(themes), () => {});
    return () => {
      live = false;
      off();
      queue.current = null;
    };
  }, [client, showPreset, reloadPreset, followTheme, toast]);

  const value = useMemo<UnitState>(
    () => ({
      client,
      preset,
      blocks,
      sending,
      connect: (next) => {
        presetRef.current = null;
        setPreset(null);
        setBlocks([]);
        setClient(next);
      },
      disconnect: async () => {
        const old = client;
        setClient(null);
        setPreset(null);
        presetRef.current = null;
        setPalette(STOCK);
        await old?.close();
      },
      edit: (edit) => {
        const edits = queue.current;
        if (!edits) return;
        // Show the change at once; the unit's reply (or a reload after a refusal) settles it.
        // The confirmed preset (presetRef) keeps the load id the unit knows.
        setPreset((shown) => {
          try {
            return shown ? applyEdit(shown, edit, blocks) : shown;
          } catch {
            return shown;
          }
        });
        setSending(true);
        edits.push(edit);
      },
      save: async () => {
        if (!client) return;
        try {
          showPreset(await client.presetSave());
          toast.ok("Preset saved");
        } catch (error) {
          toast.error(error);
        }
      },
      reloadPreset,
      followTheme,
    }),
    [client, preset, blocks, sending, reloadPreset, followTheme, setPalette, showPreset, toast],
  );

  return <UnitContext.Provider value={value}>{children}</UnitContext.Provider>;
}

export function useUnit(): UnitState {
  const state = useContext(UnitContext);
  if (!state) throw new Error("useUnit outside UnitProvider");
  return state;
}

/** The connected client; screens under the tabs only render with one. */
export function useClient(): UnitClient {
  const { client } = useUnit();
  if (!client) throw new Error("no unit connected");
  return client;
}
