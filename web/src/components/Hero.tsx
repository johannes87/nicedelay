import { AnimatePresence, motion, type MotionValue, useTransform } from 'motion/react';
import type { AppState, TransitionEvent } from '../api.ts';
import { clock, short, spoken } from '../format.ts';

const swap = {
  initial: { opacity: 0, y: 14, filter: 'blur(6px)' },
  animate: { opacity: 1, y: 0, filter: 'blur(0px)' },
  exit: { opacity: 0, y: -14, filter: 'blur(6px)' },
  transition: { type: 'spring', stiffness: 380, damping: 32 },
} as const;

interface Props {
  state: AppState | null;
  readout: MotionValue<number>;
  transition: TransitionEvent | null;
  onToggle: () => void;
  onSetup: () => void;
}

export function Hero({ state, readout, transition, onToggle, onSetup }: Props) {
  const mode = state?.playout.mode ?? 'idle';
  const delay = state?.playout.targetDelay ?? 0;
  const nextDelay = state?.delaySeconds ?? 10;
  const recorded = state?.buffer.recorded ?? 0;
  const text = useTransform(readout, (v) => clock(v));

  const big = mode === 'idle' ? 'off' : mode === 'delayed' || transition ? 'number' : 'live';

  let caption: React.ReactNode;
  if (mode === 'idle') {
    caption = (
      <>
        Start streaming in OBS to go on air.{' '}
        <button className="link" onClick={onSetup}>
          How to connect
        </button>
      </>
    );
  } else if (transition) {
    caption =
      transition.kind === 'rewind' ? 'Rewinding…' : transition.to === 0 ? 'Catching up to live…' : 'Skipping ahead…';
  } else if (mode === 'delayed') {
    caption = `Viewers see everything ${spoken(delay)} later.`;
  } else if (recorded < nextDelay) {
    caption = `Recording… ${short(recorded)} of ${short(nextDelay)} ready.`;
  } else {
    caption = 'Viewers see you in real time.';
  }

  const action = mode === 'delayed' ? 'live' : 'delay';
  const label = mode === 'idle' ? 'Waiting for OBS' : action === 'live' ? 'Go live' : `Delay ${short(nextDelay)}`;

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
        whileHover={mode === 'idle' ? undefined : { scale: 1.015 }}
        whileTap={mode === 'idle' ? undefined : { scale: 0.97 }}
        transition={{ type: 'spring', stiffness: 500, damping: 30 }}
      >
        <AnimatePresence mode="popLayout" initial={false}>
          <motion.span key={label} className="primary-label" {...swap}>
            {mode !== 'idle' && <TransportIcon direction={action === 'live' ? 'forward' : 'back'} />}
            {label}
          </motion.span>
        </AnimatePresence>
        {mode !== 'idle' && <kbd>space</kbd>}
      </motion.button>
    </div>
  );
}

function TransportIcon({ direction }: { direction: 'back' | 'forward' }) {
  return (
    <svg viewBox="0 0 24 16" className="transport" aria-hidden="true">
      {direction === 'back' ? <path d="M11 1v14L2 8zM22 1v14l-9-7z" /> : <path d="M2 1v14l9-7zM13 1v14l9-7z" />}
    </svg>
  );
}
