/** Library: IRs (converted on the phone as the Remote page does) and the files of extensions such as NAM. */

import { useCallback, useEffect, useState } from "react";
import { View } from "react-native";
import { IrError, convert, extensionFileName, irName } from "@/lib/ir/convert";
import { sendFile } from "@/lib/unit/client";
import type { ExtensionFiles, IrEntry, IrLibrary } from "@/lib/unit/types";
import { formatBytes, pickFiles } from "@/state/files";
import { errorText, useToast } from "@/state/toast";
import { useClient } from "@/state/unit";
import { useLoad } from "@/state/useLoad";
import { Icon } from "@/ui/Icon";
import { Button, Card, Empty, Pill, Row, Rows, Screen, Section, Segmented, Spinner, T, Toggle } from "@/ui/kit";
import { Sheet, TextPrompt } from "@/ui/Sheet";
import { usePalette } from "@/ui/theme";

interface Upload {
  key: string;
  name: string;
  detail: string;
  state: "sending" | "done" | "failed";
  sent?: number;
  size?: number;
}

function UploadRows({ uploads }: { uploads: Upload[] }) {
  if (uploads.length === 0) return null;
  return (
    <Rows>
      {uploads.map((u) => (
        <Row
          key={u.key}
          icon="pulse"
          title={u.name}
          detail={<T size={11} mono tone={u.state === "failed" ? "danger" : "muted"} lines={2}>{u.state === "sending" && u.size ? `${formatBytes(u.sent ?? 0)} of ${formatBytes(u.size)}` : u.detail}</T>}
          right={<Pill label={u.state === "sending" ? "Sending" : u.state === "done" ? "Added" : "Refused"} tone={u.state === "sending" ? "wait" : u.state === "done" ? "ok" : "bad"} />}
        />
      ))}
    </Rows>
  );
}

function DropBox({ title, detail, onPress, busy }: { title: string; detail: string; onPress: () => void; busy: boolean }) {
  const palette = usePalette();
  return (
    <Card style={{ backgroundColor: "transparent", borderWidth: 1.5, borderStyle: "dashed", borderColor: palette.section }}>
      <Row
        lead={
          <View style={{ width: 38, height: 38, borderRadius: 10, backgroundColor: palette.accentFill, alignItems: "center", justifyContent: "center" }}>
            <Icon name="plus" size={22} color={palette.accent} />
          </View>
        }
        title={title}
        detail={detail}
        right={busy ? <Spinner /> : undefined}
        onPress={busy ? undefined : onPress}
      />
    </Card>
  );
}

function Irs() {
  const client = useClient();
  const toast = useToast();
  const status = useLoad(() => client.status(), client);
  const lib = useLoad(() => client.library(), client);
  const [uploads, setUploads] = useState<Upload[]>([]);
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState<IrEntry | null>(null);
  const [renaming, setRenaming] = useState(false);
  const [choosing, setChoosing] = useState(false);

  const add = async () => {
    const files = await pickFiles(["audio/wav", "audio/x-wav", "audio/aiff", "audio/x-aiff", "audio/*"]);
    if (files.length === 0) return;
    setBusy(true);
    // One at a time, as the unit takes them.
    for (const [i, file] of files.entries()) {
      const key = `${Date.now()}-${i}`;
      const name = irName(file.name);
      const put = (u: Partial<Upload>) => setUploads((list) => list.map((x) => (x.key === key ? { ...x, ...u } : x)));
      setUploads((list) => [{ key, name: name || file.name, detail: "Converting…", state: "sending" } satisfies Upload, ...list].slice(0, 20));
      try {
        if (!name) throw new IrError("The file needs a name");
        const converted = convert(await file.bytes());
        put({ detail: converted.detail });
        const added = await client.uploadIr(name, converted.wav);
        put({ state: "done", name: added.name, detail: `${converted.detail} · in ${added.folder}` });
      } catch (error) {
        put({ state: "failed", detail: errorText(error) });
      }
    }
    setBusy(false);
    void status.reload();
    void lib.reload();
  };

  const op = async (run: () => Promise<IrLibrary>) => {
    try {
      lib.set(await run());
      void status.reload();
    } catch (error) {
      toast.error(error);
    }
  };

  const folders = lib.data?.folders ?? [];
  const irs = lib.data?.irs ?? [];
  const current = open ? (irs.find((i) => i.id === open.id) ?? open) : null;

  return (
    <>
      <DropBox
        title="Add from Files"
        detail={client.transport === "bluetooth" ? "WAV or AIFF · about 1 s per IR over Bluetooth" : "WAV or AIFF · converted to the unit's 48 kHz mono IR"}
        busy={busy}
        onPress={() => void add()}
      />
      <UploadRows uploads={uploads} />
      <Row
        icon="folder"
        title={<T size={13} tone="muted">Saving to <T size={13} weight="semibold">{status.data?.folder ?? "…"}</T></T>}
        chevron
        onPress={() => setChoosing(true)}
      />
      {lib.loading && !lib.data ? (
        <Spinner />
      ) : irs.length === 0 ? (
        <Empty icon="pulse" title="No IRs of yours yet" detail="IRs you add show here. Factory IRs are not listed." />
      ) : (
        <>
          <Section>{`Your IRs · ${irs.length}`}</Section>
          <Rows>
            {irs.map((ir) => (
              <Row
                key={ir.id}
                icon="pulse"
                title={ir.name}
                detail={<T size={11} mono tone="muted">{`${ir.date.slice(0, 10)} · ${ir.folders.map((f) => folders.find((x) => x.id === f)?.name ?? f).join(", ")}`}</T>}
                chevron
                onPress={() => setOpen(ir)}
              />
            ))}
          </Rows>
        </>
      )}

      <Sheet visible={!!current && !renaming} title={current?.name ?? ""} onClose={() => setOpen(null)}>
        {current && (
          <>
            <Section>Folders</Section>
            <Rows>
              {folders.map((f) => (
                <Row
                  key={f.id}
                  icon="folder"
                  title={f.name}
                  {...(f.root ? { detail: "Every IR is in the root" } : {})}
                  right={<Toggle value={current.folders.includes(f.id)} disabled={f.root} onChange={(linked) => void op(() => client.libraryOp({ op: "link", id: current.id, folder: f.id, linked }))} />}
                />
              ))}
            </Rows>
            <View style={{ flexDirection: "row", gap: 8 }}>
              <Button label="Rename" icon="pencil" style={{ flex: 1 }} onPress={() => setRenaming(true)} />
              <Button
                label="Delete"
                icon="trash"
                kind="danger"
                style={{ flex: 1 }}
                onPress={() => {
                  setOpen(null);
                  void op(() => client.libraryOp({ op: "delete", id: current.id }));
                }}
              />
            </View>
          </>
        )}
      </Sheet>
      <TextPrompt
        visible={renaming}
        title="Rename IR"
        initial={current?.name ?? ""}
        hint="4 to 60 characters."
        action="Rename"
        onClose={() => setRenaming(false)}
        onDone={(name) => {
          setRenaming(false);
          if (current) void op(() => client.libraryOp({ op: "rename", id: current.id, name }));
        }}
      />
      <Sheet visible={choosing} title="Saving destination" onClose={() => setChoosing(false)}>
        <Rows>
          {folders.map((f) => (
            <Row
              key={f.id}
              icon="folder"
              title={f.name}
              right={lib.data?.destination === f.id ? <Pill label="Here" tone="ok" /> : undefined}
              onPress={() => {
                setChoosing(false);
                void op(() => client.libraryOp({ op: "destination", folder: f.id }));
              }}
            />
          ))}
        </Rows>
      </Sheet>
    </>
  );
}

function ExtensionFilesPanel({ ext, onChange }: { ext: ExtensionFiles; onChange: () => void }) {
  const client = useClient();
  const toast = useToast();
  const [uploads, setUploads] = useState<Upload[]>([]);
  const [busy, setBusy] = useState(false);

  const add = async () => {
    const files = await pickFiles(["*/*"]);
    if (files.length === 0) return;
    setBusy(true);
    for (const [i, file] of files.entries()) {
      const key = `${Date.now()}-${i}`;
      const name = extensionFileName(file.name, ext.extensions);
      const put = (u: Partial<Upload>) => setUploads((list) => list.map((x) => (x.key === key ? { ...x, ...u } : x)));
      setUploads((list) => [{ key, name: name ?? file.name, detail: "", state: "sending", sent: 0, size: file.size } satisfies Upload, ...list].slice(0, 20));
      try {
        if (!name) throw new Error(`${ext.name} takes ${ext.extensions.join(", ")} files`);
        if (file.size > ext.maxBytes) throw new Error(`${file.name} is larger than ${formatBytes(ext.maxBytes)}`);
        const bytes = await file.bytes();
        await sendFile(client, ext.id, name, bytes, (sent) => put({ sent, size: bytes.length }));
        put({ state: "done", detail: name === file.name ? formatBytes(bytes.length) : `Stored as ${name}` });
      } catch (error) {
        put({ state: "failed", detail: errorText(error) });
      }
      onChange();
    }
    setBusy(false);
  };

  return (
    <>
      <DropBox
        title="Add from Files"
        detail={`${ext.label} (${ext.extensions.join(", ")}) · up to ${formatBytes(ext.maxBytes)}${client.transport === "bluetooth" ? " · slow over Bluetooth" : ""}`}
        busy={busy}
        onPress={() => void add()}
      />
      <UploadRows uploads={uploads} />
      {ext.files.length === 0 ? (
        <Empty icon="plugin" title={`No ${ext.label} files yet`} />
      ) : (
        <Rows>
          {ext.files.map((f) => (
            <Row
              key={f.name}
              icon="plugin"
              title={f.name}
              detail={<T size={11} mono tone={f.state === "failed" ? "danger" : "muted"} lines={2}>{f.state === "failed" ? f.detail : formatBytes(f.size)}</T>}
              right={
                <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
                  <Pill label={f.state === "ready" ? "Ready" : f.state === "waiting" ? "Reading…" : "Failed"} tone={f.state === "ready" ? "ok" : f.state === "waiting" ? "wait" : "bad"} />
                  <Button
                    icon="trash"
                    kind="ghost"
                    style={{ minHeight: 32, paddingHorizontal: 8 }}
                    onPress={() => void client.fileDelete(ext.id, f.name).then(onChange, toast.error)}
                  />
                </View>
              }
            />
          ))}
        </Rows>
      )}
    </>
  );
}

export default function Library() {
  const client = useClient();
  const files = useLoad(() => client.files(), client);
  const [tab, setTab] = useState("irs");
  const waiting = files.data?.extensions.some((e) => e.files.some((f) => f.state === "waiting"));
  const { reload } = files;
  const reloadSoon = useCallback(() => void reload(), [reload]);
  // The extension reads its folder again after an upload: ask every 2 s while a file waits.
  useEffect(() => {
    if (!waiting) return;
    const timer = setTimeout(() => void reload(), 2000);
    return () => clearTimeout(timer);
  }, [waiting, files.data, reload]);

  const options = [{ key: "irs", label: "IRs" }, ...(files.data?.extensions ?? []).map((e) => ({ key: e.id, label: e.name.replace("Neural Amp Modeler", "NAM") }))];
  const ext = files.data?.extensions.find((e) => e.id === tab);

  return (
    <Screen title="Library" right={<Pill label={client.name} tone="plain" />}>
      {options.length > 1 && <Segmented options={options} value={tab} onChange={setTab} />}
      {tab === "irs" || !ext ? <Irs /> : <ExtensionFilesPanel ext={ext} onChange={reloadSoon} />}
    </Screen>
  );
}
