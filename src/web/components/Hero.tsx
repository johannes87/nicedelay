import { AnimatePresence, motion, type MotionValue, useTransform } from 'motion/react';
import type { AppState, TransitionEvent } from '../api.ts';
import { clock, short, spoken } from '../format.ts';

const swap = {
  initial: { opacity: 0, y: 14, filter: 'blur(6px)' },
  animate: { opacity: 1, y: 0, filter: 'blur(0px)' },
  exit: { opacity: 0, y: -14, filter: 'blur(6px)' },
  transition: { type: 'spring', stiffness: 380, damping: 32 },
} as const;

type Props = Readonly<{
  state: AppState | null;
  readout: MotionValue<number>;
  transition: TransitionEvent | null;
  onToggle: () => void;
  onSetup: () => void;
}>;

type Mode = AppState['playout']['mode'];

/** Which big readout to show: off air, the pulsing LIVE, or the delay counter. */
function readoutKind(mode: Mode, transition: TransitionEvent | null): 'off' | 'live' | 'number' {
  if (mode === 'idle') return 'off';
  return mode === 'delayed' || transition ? 'number' : 'live';
}

function transitionCaption(t: TransitionEvent): string {
  if (t.kind === 'rewind') return 'Rewinding…';
  return t.to === 0 ? 'Catching up to live…' : 'Skipping ahead…';
}

function statusCaption(state: AppState | null, transition: TransitionEvent | null): string {
  const mode = state?.playout.mode ?? 'idle';
  const nextDelay = state?.delaySeconds ?? 10;
  const recorded = state?.buffer.recorded ?? 0;
  if (transition) return transitionCaption(transition);
  if (mode === 'delayed') return `Viewers see everything ${spoken(state?.playout.targetDelay ?? 0)} later.`;
  if (recorded < nextDelay) return `Recording… ${short(recorded)} of ${short(nextDelay)} ready.`;
  return 'Viewers see you in real time.';
}

function buttonLabel(mode: Mode, nextDelay: number): string {
  if (mode === 'idle') return 'Waiting for OBS';
  return mode === 'delayed' ? 'Go live' : `Delay ${short(nextDelay)}`;
}

/** How the stream ends, shown while on air. */
function endHint(state: AppState | null): string | null {
  if (!state || state.playout.mode === 'idle') return null;
  if (!state.ingest.connected) return 'OBS stopped. The stream ends automatically.';
  return 'To end the stream, stop streaming in OBS.';
}

export function Hero({ state, readout, transition, onToggle, onSetup }: Props) {
  const mode = state?.playout.mode ?? 'idle';
  const text = useTransform(readout, (v) => clock(v));
  const big = readoutKind(mode, transition);
  const caption =
    mode === 'idle' ? (
      <>
        Start streaming in OBS to go on air.{' '}
        <button className="link" onClick={onSetup}>
          How to connect
        </button>
      </>
    ) : (
      statusCaption(state, transition)
    );
  const action = mode === 'delayed' ? 'live' : 'delay';
  const label = buttonLabel(mode, state?.delaySeconds ?? 10);
  const interactive = mode !== 'idle';

  return (
    <div className="hero">
      <div className="readout">
        <AnimatePresence>
          {transition && (
            <motion.span
              key={`${transition.from}-${transition.to}-${transition.kind}`}
              className="ripple"
              initial={{ scale: 0.6, opacity: 0.55 }}
              animate={{ scale: 2.2, opacity: 0 }}
              exit={{ opacity: 0 }}
              transition={{ duration: 1.1, ease: 'easeOut' }}
            />
          )}
        </AnimatePresence>
        <AnimatePresence mode="popLayout" initial={false}>
          {big === 'off' && (
            <motion.div key="off" className="readout-text off" {...swap}>
              Off air
            </motion.div>
          )}
          {big === 'live' && (
            <motion.div key="live" className="readout-text live" {...swap}>
              <span className="live-dot" />
              LIVE
            </motion.div>
          )}
          {big === 'number' && (
            <motion.div key="number" className="readout-text number" {...swap}>
              {text}
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      <AnimatePresence mode="wait" initial={false}>
        <motion.p
          key={typeof caption === 'string' ? caption : 'idle'}
          className="caption"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.18 }}
        >
          {caption}
        </motion.p>
      </AnimatePresence>

      <motion.button
        className={`primary ${action}`}
        disabled={mode === 'idle'}
        onClick={onToggle}
        whileHover={interactive ? { scale: 1.015 } : undefined}
        whileTap={interactive ? { scale: 0.97 } : undefined}
        transition={{ type: 'spring', stiffness: 500, damping: 30 }}
      >
        <AnimatePresence mode="popLayout" initial={false}>
          <motion.span key={label} className="primary-label" {...swap}>
            {interactive && <TransportIcon direction={action === 'live' ? 'forward' : 'back'} />}
            {label}
          </motion.span>
        </AnimatePresence>
        {interactive && <kbd>space</kbd>}
      </motion.button>
      <EndHint text={endHint(state)} />
    </div>
  );
}

function EndHint({ text }: Readonly<{ text: string | null }>) {
  return (
    <AnimatePresence mode="wait" initial={false}>
      {text && (
        <motion.p
          key={text}
          className="end-hint"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.2 }}
        >
          {text}
        </motion.p>
      )}
    </AnimatePresence>
  );
}

function TransportIcon({ direction }: Readonly<{ direction: 'back' | 'forward' }>) {
  return (
    <svg viewBox="0 0 24 16" className="transport" aria-hidden="true">
      {direction === 'back' ? <path d="M11 1v14L2 8zM22 1v14l-9-7z" /> : <path d="M2 1v14l9-7zM13 1v14l9-7z" />}
    </svg>
  );
}
