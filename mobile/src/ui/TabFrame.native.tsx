import { useRef } from "react";
import { WebView } from "react-native-webview";
import { framedPage, parseRequest, replyScript } from "./tab-bridge";

/** An extension's page in a WebView that cannot leave the page. */
export function TabFrame({ html, call }: { html: string; call: (message: unknown) => Promise<unknown> }) {
  // Its typings make the props `never` without a props argument.
  const view = useRef<WebView<object>>(null);
  return (
    <WebView<object>
      ref={view}
      originWhitelist={["about:blank"]}
      source={{ html: framedPage(html, "window.ReactNativeWebView.postMessage") }}
      onShouldStartLoadWithRequest={(request) => request.url === "about:blank" || request.url.startsWith("data:")}
      javaScriptCanOpenWindowsAutomatically={false}
      setSupportMultipleWindows={false}
      allowFileAccess={false}
      style={{ flex: 1, backgroundColor: "transparent" }}
      onMessage={(event) => {
        const request = parseRequest(event.nativeEvent.data);
        if (!request) return;
        call(request.message).then(
          (value) => view.current?.injectJavaScript(replyScript(request.id, true, value)),
          (error: unknown) => view.current?.injectJavaScript(replyScript(request.id, false, error instanceof Error ? error.message : String(error))),
        );
      }}
    />
  );
}
