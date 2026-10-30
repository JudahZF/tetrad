import { useCallback, useEffect, useRef, useState } from "react";
import { errorText } from "./toast";

/**
 * Loads a view from the unit and keeps it. Commands reply with the whole new view, so `set`
 * takes their reply; `reload` asks again. It loads again when `key` or `key2` change (the client,
 * an id), and every `every` ms while the screen shows.
 */
export function useLoad<T>(load: () => Promise<T>, key: unknown, key2?: unknown, every?: number) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const latest = useRef(load);
  const live = useRef(true);
  useEffect(() => {
    latest.current = load;
  });

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const next = await latest.current();
      if (live.current) {
        setData(next);
        setError(null);
      }
    } catch (e) {
      if (live.current) setError(errorText(e));
    } finally {
      if (live.current) setLoading(false);
    }
    // `key` and `key2` say when the view is another one.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, key2]);

  useEffect(() => {
    live.current = true;
    const first = setTimeout(() => void reload(), 0);
    const timer = every ? setInterval(() => void reload(), every) : undefined;
    return () => {
      live.current = false;
      clearTimeout(first);
      clearInterval(timer);
    };
  }, [reload, every]);

  return { data, error, loading, reload, set: setData };
}
