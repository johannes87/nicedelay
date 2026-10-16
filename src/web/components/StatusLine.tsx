import type { AppState } from '../api.ts';
import { clock } from '../format.ts';

type Props = Readonly<{
  state: AppState | null;
  online: boolean;
  onOpenSettings: () => void;
}>;

function obsLabel({ connected, info }: AppState['ingest']): string {
  if (!connected) return 'Waiting for OBS';
  if (!info) return 'OBS connected';
  const format = `${info.height}p${Math.round(info.fps)}`;
  return `OBS connected · ${format}`;
}

/** One quiet line: is OBS here, where is it going, how much is recorded. */
export function StatusLine({ state, online, onOpenSettings }: Props) {
  if (!online || !state) {
    return (
      <footer className="status">
        <Item tone="bad">Can’t reach nicedelay — is it still running?</Item>
      </footer>
    );
  }
  const { ingest, playout, buffer } = state;
  const out = playout.output;

  let destination: React.ReactNode;
  if (out.error && out.running) destination = <Item tone="bad">{out.error}</Item>;
  else if (out.running && out.previewOnly)
    destination = (
      <Item tone="warn">
        Not streaming anywhere yet —{' '}
        <button className="link" onClick={onOpenSettings}>
          add a destination
        </button>
      </Item>
    );
  else if (out.running) destination = <Item tone="good">Streaming to {out.destinations.join(', ')}</Item>;

  return (
    <footer className="status">
      <Item tone={ingest.connected ? 'good' : 'idle'}>{obsLabel(ingest)}</Item>
      {destination}
      {buffer.recorded > 0 && (
        <Item>
          {clock(buffer.recorded, false)} of {clock(buffer.capacity, false)} recorded
        </Item>
      )}
    </footer>
  );
}

function Item({ tone, children }: Readonly<{ tone?: 'good' | 'warn' | 'bad' | 'idle'; children: React.ReactNode }>) {
  return (
    <span className="status-item">
      {tone && <i className={`status-dot ${tone}`} />}
      <span>{children}</span>
    </span>
  );
}
