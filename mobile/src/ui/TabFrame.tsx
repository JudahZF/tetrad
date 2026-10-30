import { createElement, useEffect, useState } from "react";
import { framedPage, parseRequest } from "./tab-bridge";

/** In the frame: replies from the parent settle `tetrad.send`. */
const LISTENER = `<script>window.__post=function(m){parent.postMessage(m,"*");};window.addEventListener("message",function(e){if(e.source!==parent||typeof e.data!=="string")return;
try{var d=JSON.parse(e.data);window.__tetradReply(d.id,d.ok,d.value);}catch(_){}});</script>`;

/** Web: an extension's page in a sandboxed iframe (scripts only, no same origin, no network). */
export function TabFrame({ html, call }: { html: string; call: (message: unknown) => Promise<unknown> }) {
  const [frame, setFrame] = useState<HTMLIFrameElement | null>(null);
  useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      if (!frame || event.source !== frame.contentWindow) return;
      const request = parseRequest(event.data);
      if (!request) return;
      const answer = (ok: boolean, value: unknown) =>
        frame.contentWindow?.postMessage(JSON.stringify({ id: request.id, ok, value: value ?? null }), "*");
      call(request.message).then(
        (value) => answer(true, value),
        (error: unknown) => answer(false, error instanceof Error ? error.message : String(error)),
      );
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [call, frame]);
  return createElement("iframe", {
    ref: setFrame,
    srcDoc: framedPage(LISTENER + html, "window.__post"),
    sandbox: "allow-scripts",
    style: { flex: 1, border: 0, width: "100%", height: "100%", background: "transparent" },
    title: "Extension page",
  });
}
