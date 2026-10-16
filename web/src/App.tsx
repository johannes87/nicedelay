import { animate, useMotionValue } from 'motion/react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { api, PREVIEW_LATENCY, type TransitionEvent, useServer } from './api.ts';
import { DelayPicker } from './components/DelayPicker.tsx';
import { Hero } from './components/Hero.tsx';
import { Preview } from './components/Preview.tsx';
import { Settings } from './components/Settings.tsx';
import { StatusLine } from './components/StatusLine.tsx';
import { Toast, useToast } from './components/Toast.tsx';
import { clock } from './format.ts';

/** Same curve as the on-stream scrub (cubic ease-in-out), so both move in lockstep. */
const SCRUB_EASE = [0.65, 0, 0.35, 1] as const;

export function App() {
  const readout = useMotionValue(0);
  const [transition, setTransition] = useState<TransitionEvent | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const toast = useToast();
  const endTimer = useRef<number | undefined>(undefined);

  const onTransition = useCallback(
    (t: TransitionEvent) => {
      setTransition(t);
      readout.set(t.from);
      const delay = PREVIEW_LATENCY + t.phases.intro;
      void animate(readout, t.to, { delay, duration: t.phases.scrub, ease: SCRUB_EASE });
      window.clearTimeout(endTimer.current);
      const total = (PREVIEW_LATENCY + t.phases.intro + t.phases.scrub + t.phases.land) * 1000;
      endTimer.current = window.setTimeout(() => setTransition(null), total);
    },
    [readout],
  );

  const { state, online } = useServer(onTransition);
  const mode = state?.playout.mode ?? 'idle';
  const delay = state?.playout.targetDelay ?? 0;

  // Outside of transitions the readout simply mirrors the server.
  useEffect(() => {
    if (!transition) readout.set(mode === 'delayed' ? delay : 0);
  }, [transition, mode, delay, readout]);

  const run = useCallback(
    async (action: () => ReturnType<typeof api.toggle>) => {
      try {
        const res = await action();
        if (!res.ok) toast.show(res.error ?? 'That didn’t work.');
        else if (res.clamped && res.delay) toast.show(`Only ${clock(res.delay, false)} recorded so far — using that.`);
      } catch (err) {
        toast.show(err instanceof Error ? err.message : 'Lost connection to nicedelay.');
      }
    },
    [toast],
  );

  const toggle = useCallback(() => void run(api.toggle), [run]);

  const pickDelay = useCallback(
    (seconds: number) => {
      void (async () => {
        await api.saveConfig({ delaySeconds: seconds }).catch(() => undefined);
        if (mode === 'delayed') await run(() => api.delay(seconds));
      })();
    },
    [mode, run],
  );

  // Space toggles, L goes live — unless typing or a dialog is open.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (settingsOpen || e.metaKey || e.ctrlKey || e.altKey) return;
      const el = e.target as HTMLElement;
      if (el.closest('input, select, textarea, button, [contenteditable]') && e.key !== 'l') return;
      if (e.key === ' ') {
        e.preventDefault();
        toggle();
      } else if (e.key === 'l' || e.key === 'L') {
        void run(api.live);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [settingsOpen, toggle, run]);

  return (
    <div className="app" data-mode={mode}>
      <header className="bar">
        <div className="brand">
          <Logo />
          nicedelay
        </div>
        <button className="icon-button" onClick={() => setSettingsOpen(true)} aria-label="Settings" title="Settings">
          <GearIcon />
        </button>
      </header>

      <main className="main">
        <Preview running={!!state?.playout.output.running} mode={mode} transition={transition} />
        <section className="controls">
          <Hero
            state={state}
            readout={readout}
            transition={transition}
            onToggle={toggle}
            onSetup={() => setSettingsOpen(true)}
          />
          <DelayPicker state={state} onPick={pickDelay} />
        </section>
      </main>

      <StatusLine state={state} online={online} onOpenSettings={() => setSettingsOpen(true)} />
      <Settings
        open={settingsOpen}
        state={state}
        onClose={() => setSettingsOpen(false)}
        onSaved={(restart) =>
          restart && toast.show('Saved. Quality and connection changes apply when your stream ends.')
        }
      />
      <Toast message={toast.message} />
    </div>
  );
}

function Logo() {
  return (
    <svg viewBox="0 0 32 32" className="logo" aria-hidden="true">
      <circle cx="16" cy="16" r="15" />
      <path d="M15 9v14L6 16zM25 9v14l-9-7z" />
    </svg>
  );
}

function GearIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h0a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v0a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z" />
    </svg>
  );
}
