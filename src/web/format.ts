/** −0:10, −1:30, −1:02:00 */
export function clock(seconds: number, sign = true): string {
  const s = Math.max(0, Math.round(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  const body = h ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
  return sign ? `−${body}` : body;
}

/** 5s, 30s, 1m, 1m 30s */
export function short(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return s ? `${m}m ${s}s` : `${m}m`;
}

const plural = (n: number, unit: string) => `${n} ${unit}${n === 1 ? '' : 's'}`;

/** "10 seconds", "1 minute", "2 minutes 30 seconds" */
export function spoken(seconds: number): string {
  const s = Math.round(seconds);
  if (s < 60) return plural(s, 'second');
  const m = Math.floor(s / 60);
  const rest = s % 60;
  return rest
    ? `${plural(m, 'minute')} ${plural(rest, 'second')}`
    : plural(m, 'minute');
}
