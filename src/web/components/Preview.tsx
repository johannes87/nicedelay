import { motion, useAnimate } from 'motion/react';
import {
  type CSSProperties,
  type PointerEvent,
  useEffect,
  useRef,
  useState,
} from 'react';
import {
  api,
  type BadgePosition,
  PREVIEW_LATENCY,
  type TransitionEvent,
} from '../api.ts';

type Props = Readonly<{
  running: boolean;
  mode: string;
  transition: TransitionEvent | null;
  /** output frame size, for placing the badge handle over the picture */
  size: { width: number; height: number };
  badge: BadgePosition;
  badgeText: string;
}>;

/** What viewers see: the encoder's own input, including the on-stream animation. */
export function Preview({
  running,
  mode,
  transition,
  size,
  badge,
  badgeText,
}: Props) {
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
      <div
        className={running ? 'badge-area on-air' : 'badge-area'}
        style={{ '--ar': size.width / size.height } as CSSProperties}
      >
        <BadgeHandle
          position={badge}
          text={badgeText}
          dot={mode !== 'delayed'}
        />
      </div>
    </motion.div>
  );
}

/** Same margin as the on-stream badge, in 1080p pixels. */
const BADGE_MARGIN = 36;
/** Positions this close to an edge or the centre snap to it. */
const SNAP = 0.02;

function snap(v: number): number {
  const c = Math.min(1, Math.max(0, v));
  return [0, 0.5, 1].find((t) => Math.abs(c - t) < SNAP) ?? c;
}

/** An outline of the on-stream badge that appears on hover; dragging it moves the badge on the stream. */
function BadgeHandle({
  position,
  text,
  dot,
}: Readonly<{ position: BadgePosition; text: string; dot: boolean }>) {
  const [drag, setDrag] = useState<BadgePosition | null>(null);
  const grab = useRef<{ dx: number; dy: number } | null>(null);
  const pos = drag ?? position;

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    grab.current = { dx: e.clientX - r.left, dy: e.clientY - r.top };
    e.currentTarget.setPointerCapture(e.pointerId);
    setDrag(position);
  };

  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const area = e.currentTarget.parentElement?.getBoundingClientRect();
    if (!grab.current || !area) return;
    const own = e.currentTarget.getBoundingClientRect();
    const m = (BADGE_MARGIN / 1080) * area.height;
    const left = e.clientX - grab.current.dx - area.left;
    const top = e.clientY - grab.current.dy - area.top;
    setDrag({
      x: snap((left - m) / (area.width - 2 * m - own.width)),
      y: snap((top - m) / (area.height - 2 * m - own.height)),
    });
  };

  const onPointerUp = () => {
    if (!grab.current) return;
    grab.current = null;
    if (!drag) return;
    void api
      .saveConfig({ badge: drag })
      .catch(() => undefined)
      .finally(() => setDrag(null));
  };

  return (
    <div
      className={drag ? 'badge-handle dragging' : 'badge-handle'}
      style={{ '--x': pos.x, '--y': pos.y } as CSSProperties}
      title="Drag to move the delay badge"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
    >
      {dot && <span className="badge-dot" />}
      {text}
    </div>
  );
}
