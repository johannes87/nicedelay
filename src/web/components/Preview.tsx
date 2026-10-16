import { motion, useAnimate } from 'motion/react';
import { useEffect, useState } from 'react';
import { PREVIEW_LATENCY, type TransitionEvent } from '../api.ts';

type Props = Readonly<{
  running: boolean;
  mode: string;
  transition: TransitionEvent | null;
}>;

/** What viewers see: the encoder's own input, including the on-stream animation. */
export function Preview({ running, mode, transition }: Props) {
  const [scope, animate] = useAnimate<HTMLDivElement>();
  const [attempt, setAttempt] = useState(0);

  // A soft "tape" dip while the stream rewinds or fast-forwards.
  useEffect(() => {
    if (!transition || !scope.current) return;
    const { intro, scrub, land } = transition.phases;
    const total = intro + scrub + land;
    void animate(
      scope.current,
      { scale: [1, 0.975, 0.975, 1] },
      {
        delay: PREVIEW_LATENCY,
        duration: total,
        times: [0, intro / total, (intro + scrub) / total, 1],
        ease: 'easeInOut',
      },
    );
  }, [transition, animate, scope]);

  return (
    <motion.div ref={scope} className="preview">
      {running ? (
        <img
          key={attempt}
          src={`/preview.mjpg?${attempt}`}
          alt="Output preview"
          onError={() =>
            window.setTimeout(() => setAttempt((a) => a + 1), 1000)
          }
        />
      ) : (
        <div className="preview-empty">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <rect x="2.5" y="5" width="14" height="14" rx="3" />
            <path d="M16.5 10.5 21.5 7v10l-5-3.5" />
          </svg>
          <span>
            {mode === 'idle' ? 'Your stream will appear here' : 'Starting…'}
          </span>
        </div>
      )}
    </motion.div>
  );
}
