import { motion } from 'motion/react';
import type { AppState } from '../api.ts';
import { short } from '../format.ts';

const OPTIONS = [5, 10, 30, 60, 120, 300];

/** Segmented control for how far behind viewers should be. */
export function DelayPicker({ state, onPick }: { state: AppState | null; onPick: (seconds: number) => void }) {
  const capacity = state?.buffer.capacity ?? 600;
  const options = OPTIONS.filter((s) => s <= capacity - 2);
  const delayed = state?.playout.mode === 'delayed';
  const selected = delayed ? state.playout.targetDelay : (state?.delaySeconds ?? 10);

  return (
    <div className="picker">
      <span className="picker-label">Delay</span>
      <div className="segmented" role="radiogroup" aria-label="Delay">
        {options.map((s) => (
          <button
            key={s}
            role="radio"
            aria-checked={s === selected}
            className={s === selected ? 'segment selected' : 'segment'}
            onClick={() => onPick(s)}
          >
            {s === selected && (
              <motion.span
                layoutId="segment-highlight"
                className="segment-highlight"
                transition={{ type: 'spring', stiffness: 500, damping: 38 }}
              />
            )}
            <span className="segment-text">{short(s)}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
