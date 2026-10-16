import { useEffect, useEffectEvent, useState } from 'react';
import type {
  AppState,
  CommandResult,
  Config,
  ConfigResponse,
  TransitionEvent,
} from '../shared/types.ts';

export type {
  AppState,
  BadgePosition,
  Config,
  Destination,
  TransitionEvent,
} from '../shared/types.ts';

/** The MJPEG preview reaches the page ~0.3 s after the server renders a frame; UI animations wait for it. */
export const PREVIEW_LATENCY = 0.3;

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  const res = await fetch(path, {
    method,
    headers:
      body === undefined ? undefined : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const data = (await res.json()) as T & { error?: string };
  if (!res.ok && res.status !== 409)
    throw new Error(data.error ?? `Request failed (${res.status})`);
  return data;
}

export const api = {
  toggle: () => request<CommandResult>('POST', '/api/toggle'),
  delay: (seconds: number) =>
    request<CommandResult>('POST', '/api/delay', { seconds }),
  live: () => request<CommandResult>('POST', '/api/live'),
  config: () => request<ConfigResponse>('GET', '/api/config'),
  saveConfig: (config: Partial<Config>) =>
    request<ConfigResponse>('PUT', '/api/config', config),
};

/** Live server state over Server-Sent Events, plus a callback for transition events. */
export function useServer(onTransition: (t: TransitionEvent) => void): {
  state: AppState | null;
  online: boolean;
} {
  const [state, setState] = useState<AppState | null>(null);
  const [online, setOnline] = useState(true);
  const handleTransition = useEffectEvent(onTransition);

  useEffect(() => {
    const es = new EventSource('/api/events');
    es.addEventListener('state', (e) => {
      setState(JSON.parse((e as MessageEvent<string>).data) as AppState);
      setOnline(true);
    });
    es.addEventListener('transition', (e) => {
      handleTransition(
        JSON.parse((e as MessageEvent<string>).data) as TransitionEvent,
      );
    });
    es.addEventListener('error', () => setOnline(false));
    return () => es.close();
  }, []);

  return { state, online };
}
