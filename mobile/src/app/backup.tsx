/** Backup: the unit packs a backup and the phone keeps it; or the phone sends one back for the unit to restore. */

import { router } from "expo-router";
import { useEffect, useState } from "react";
import type { BackupStatus } from "@/lib/unit/types";
import { FILE_PIECE } from "@/lib/unit/client";
import { formatBytes, pickFiles, shareFile } from "@/state/files";
import { errorText, useToast } from "@/state/toast";
import { useClient } from "@/state/unit";
import { useLoad } from "@/state/useLoad";
import { Button, Card, Pill, Row, Rows, Screen, Spinner, T } from "@/ui/kit";

const STATE_TEXT: Record<BackupStatus["state"], string> = {
  idle: "No backup waits on the unit.",
  creating: "The unit packs the backup…",
  ready: "The backup is packed. Save it to the phone.",
  receiving: "A backup is on its way to the unit…",
  received: "The unit has the backup. Touch RESTORE on the unit to restore it, or discard it.",
  restoring: "The unit restores the backup…",
  restored: "The unit restored the backup.",
};

export default function Backup() {
  const client = useClient();
  const toast = useToast();
  const status = useLoad(() => client.backup(), client);
  const [progress, setProgress] = useState<string | null>(null);
  const state = status.data?.state;
  const { reload } = status;

  // The unit packs and restores on its own: ask once a second meanwhile.
  useEffect(() => {
    if (state !== "creating" && state !== "restoring") return;
    const timer = setInterval(() => void reload(), 1000);
    return () => clearInterval(timer);
  }, [state, reload]);

  const run = async (work: () => Promise<void>) => {
    try {
      await work();
    } catch (error) {
      toast.error(error);
    } finally {
      setProgress(null);
      void reload();
    }
  };

  const download = () =>
    run(async () => {
      const ready = status.data;
      if (!ready?.size || !ready.name) return;
      const out = new Uint8Array(ready.size);
      for (let offset = 0; offset < ready.size; ) {
        setProgress(`${formatBytes(offset)} of ${formatBytes(ready.size)}`);
        const piece = await client.backupPiece(offset);
        if (piece.length === 0) throw new Error("The backup ended early");
        out.set(piece.subarray(0, ready.size - offset), offset);
        offset += piece.length;
      }
      await shareFile(ready.name, out, "application/gzip");
      // The unit keeps no copy once the phone has it.
      status.set(await client.backupDiscard());
      toast.ok("Backup saved");
    });

  const upload = () =>
    run(async () => {
      const [file] = await pickFiles(["application/gzip", "application/x-gzip", "application/octet-stream", "*/*"]);
      if (!file) return;
      const bytes = await file.bytes();
      if (bytes[0] !== 0x1f || bytes[1] !== 0x8b) throw new Error("A backup is a .tar.gz file from Tetrad");
      let last: BackupStatus | null = null;
      for (let offset = 0; offset < bytes.length; offset += FILE_PIECE) {
        setProgress(`${formatBytes(offset)} of ${formatBytes(bytes.length)}`);
        const piece = bytes.subarray(offset, offset + FILE_PIECE);
        last = await client.backupUpload(file.name, offset, offset + piece.length >= bytes.length, piece);
      }
      if (last) status.set(last);
    });

  const data = status.data;
  return (
    <Screen title="Backup" right={<Button icon="close" kind="ghost" accessibilityLabel="Close" onPress={() => router.back()} />}>
      <Card style={{ padding: 14, gap: 8 }}>
        {data ? (
          <>
            <T size={14} weight="semibold">
              {STATE_TEXT[data.state]}
            </T>
            {data.name && (
              <T size={12} mono tone="muted">
                {`${data.name}${data.size ? ` · ${formatBytes(data.size)}` : ""}${data.received ? ` · ${formatBytes(data.received)} received` : ""}`}
              </T>
            )}
            {data.error && <T size={12.5} tone="danger">{data.error}</T>}
            {progress && <Pill label={progress} tone="wait" />}
          </>
        ) : status.error ? (
          <T tone="danger">{status.error ?? errorText("")}</T>
        ) : (
          <Spinner />
        )}
      </Card>
      <Rows>
        <Row
          icon="archive"
          title="Back up now"
          detail="Presets, IRs, captures and settings, as one file for the phone"
          disabled={!!progress || state === "creating" || state === "restoring" || state === "received"}
          onPress={() => void run(async () => status.set(await client.backupCreate()))}
        />
        {state === "ready" && <Row icon="download" title="Save to the phone" detail="Then the unit drops its copy" disabled={!!progress} onPress={() => void download()} />}
        <Row icon="refresh" title="Restore from the phone" detail="Sends a backup; the unit asks before it restores" disabled={!!progress || state === "creating" || state === "restoring"} onPress={() => void upload()} />
        {(state === "ready" || state === "received") && <Row icon="trash" title="Discard" detail="Drop the backup that waits on the unit" onPress={() => void run(async () => status.set(await client.backupDiscard()))} />}
      </Rows>
      <T size={11.5} tone="faint" center>
        A restore starts only with a touch on the unit.
      </T>
    </Screen>
  );
}
