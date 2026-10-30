/**
 * An extension's page runs with no network and no access to the app: its only way to the unit
 * is `tetrad.send(message)`, which becomes a tab call to its own extension (as on the Remote
 * page, crates/tetrad-remote/src/page.html).
 */

/** No network, no frames, no forms: inline script, style and data images only. */
const POLICY =
  "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; form-action 'none'";

/** The page with its policy and the bridge before the extension's own HTML. `post` sends a JSON string to the app. */
export function framedPage(html: string, post: string): string {
  const bridge = `<script>(function(){var next=1,wait={};
window.tetrad={send:function(message){var id=next++;return new Promise(function(resolve,reject){wait[id]={resolve:resolve,reject:reject};
${post}(JSON.stringify({id:id,message:message===undefined?null:message}));});}};
window.__tetradReply=function(id,ok,value){var w=wait[id];if(!w)return;delete wait[id];ok?w.resolve(value):w.reject(new Error(value));};})();</script>`;
  return `<!doctype html><meta http-equiv="Content-Security-Policy" content="${POLICY}"><meta name="viewport" content="width=device-width, initial-scale=1">${bridge}${html}`;
}

/** A request from the page, or null for anything else. */
export function parseRequest(data: unknown): { id: number; message: unknown } | null {
  if (typeof data !== "string") return null;
  try {
    const parsed: unknown = JSON.parse(data);
    if (parsed && typeof parsed === "object" && "id" in parsed && typeof parsed.id === "number" && "message" in parsed) {
      return { id: parsed.id, message: parsed.message };
    }
  } catch {}
  return null;
}

/** Script that settles request `id` in the page. */
export const replyScript = (id: number, ok: boolean, value: unknown) =>
  `window.__tetradReply(${id},${ok},${JSON.stringify(value ?? null)});true;`;
