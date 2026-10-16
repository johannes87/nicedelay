import { useEffect, useRef, useState } from 'react';
import { type AppState, api, type Config, type Destination } from '../api.ts';

const SERVICES = [
  // Secure (RTMPS) ingest, so stream keys never cross the network in clear text.
  {
    name: 'Twitch',
    url: 'rtmps://ingest.global-contribute.live-video.net/app',
  },
  { name: 'YouTube', url: 'rtmps://a.rtmps.youtube.com/live2' },
] as const;

const RESOLUTIONS = [
  { label: '720p', width: 1280, height: 720 },
  { label: '1080p', width: 1920, height: 1080 },
  { label: '1440p', width: 2560, height: 1440 },
];

const BUFFER_MINUTES = [2, 5, 10, 15, 30];

// Stable React keys for destination rows (the config itself has no ids).
const rowKeys = new WeakMap<Destination, number>();
let nextRowKey = 1;
function rowKey(d: Destination): number {
  let k = rowKeys.get(d);
  if (k === undefined) {
    k = nextRowKey++;
    rowKeys.set(d, k);
  }
  return k;
}

type Props = Readonly<{
  open: boolean;
  state: AppState | null;
  onClose: () => void;
  onSaved: (restartPending: boolean) => void;
}>;

export function Settings({ open, state, onClose, onSaved }: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [draft, setDraft] = useState<Config | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    const d = dialog.current;
    if (!d) return;
    if (open && !d.open) {
      d.showModal();
      setDraft(null);
      api
        .config()
        .then((r) => setDraft(r.config))
        .catch(() => onClose());
    } else if (!open && d.open) {
      d.close();
    }
  }, [open, onClose]);

  const set = (patch: Partial<Config>) =>
    setDraft((d) => (d ? { ...d, ...patch } : d));
  // Edited rows keep their React key so inputs don't lose focus.
  const setDest = (i: number, patch: Partial<Destination>) =>
    setDraft(
      (d) =>
        d && {
          ...d,
          destinations: d.destinations.map((x, j) => {
            if (j !== i) return x;
            const next = { ...x, ...patch };
            rowKeys.set(next, rowKey(x));
            return next;
          }),
        },
    );

  const save = async () => {
    if (!draft) return;
    setSaving(true);
    try {
      const res = await api.saveConfig(draft);
      onSaved(res.restartPending);
      onClose();
    } finally {
      setSaving(false);
    }
  };

  return (
    <dialog ref={dialog} className="sheet" onClose={onClose} onCancel={onClose}>
      <div className="sheet-head">
        <button className="text-button" onClick={onClose}>
          Cancel
        </button>
        <h2>Settings</h2>
        <button
          className="text-button strong"
          onClick={() => void save()}
          disabled={!draft || saving}
        >
          Done
        </button>
      </div>

      {draft && (
        <div className="sheet-body">
          <Group
            title="Connect OBS"
            note="In OBS, open Settings → Stream, choose Custom, and paste these."
          >
            <CopyRow label="Server" value={state?.ingest.server ?? ''} />
            <CopyRow label="Stream key" value={draft.ingest.key} />
          </Group>

          <Group
            title="Stream to"
            note="nicedelay sends the delayed stream here — instead of OBS."
          >
            {draft.destinations.length === 0 && (
              <p className="empty">No destinations yet.</p>
            )}
            {draft.destinations.map((d, i) => {
              const known = SERVICES.find((s) => s.url === d.url);
              return (
                <div className="dest" key={rowKey(d)}>
                  <Switch
                    checked={d.enabled}
                    onChange={(enabled) => setDest(i, { enabled })}
                    label={`Stream to ${d.name || d.url}`}
                  />
                  <div className="dest-fields">
                    {known ? (
                      <span className="dest-name">{known.name}</span>
                    ) : (
                      <input
                        value={d.url}
                        placeholder="rtmps://server/app"
                        aria-label="Server URL"
                        onChange={(e) => setDest(i, { url: e.target.value })}
                      />
                    )}
                    <input
                      type="password"
                      value={d.key}
                      placeholder="Stream key"
                      aria-label="Stream key"
                      autoComplete="off"
                      onChange={(e) => setDest(i, { key: e.target.value })}
                    />
                  </div>
                  <button
                    className="remove"
                    aria-label="Remove"
                    onClick={() =>
                      set({
                        destinations: draft.destinations.filter(
                          (_, j) => j !== i,
                        ),
                      })
                    }
                  >
                    ×
                  </button>
                </div>
              );
            })}
            <div className="add-row">
              {SERVICES.map((s) => (
                <button
                  key={s.name}
                  className="pill-button"
                  onClick={() =>
                    set({
                      destinations: [
                        ...draft.destinations,
                        { name: s.name, url: s.url, key: '', enabled: true },
                      ],
                    })
                  }
                >
                  + {s.name}
                </button>
              ))}
              <button
                className="pill-button"
                onClick={() =>
                  set({
                    destinations: [
                      ...draft.destinations,
                      { name: '', url: '', key: '', enabled: true },
                    ],
                  })
                }
              >
                + Other
              </button>
            </div>
          </Group>

          <Group title="Quality">
            <Row label="Resolution">
              <Choice
                options={RESOLUTIONS.map((r) => r.label)}
                value={
                  RESOLUTIONS.find((r) => r.height === draft.output.height)
                    ?.label ?? '1080p'
                }
                onChange={(label) => {
                  const r =
                    RESOLUTIONS.find((x) => x.label === label) ??
                    RESOLUTIONS[1];
                  set({
                    output: {
                      ...draft.output,
                      width: r.width,
                      height: r.height,
                    },
                  });
                }}
              />
            </Row>
            <Row label="Frame rate">
              <Choice
                options={['30', '60']}
                value={String(draft.output.fps)}
                onChange={(fps) =>
                  set({ output: { ...draft.output, fps: Number(fps) } })
                }
              />
            </Row>
            <Row label="Bitrate">
              <span className="unit-input">
                <input
                  type="number"
                  min={500}
                  max={50000}
                  step={500}
                  value={draft.output.videoBitrate}
                  onChange={(e) =>
                    set({
                      output: {
                        ...draft.output,
                        videoBitrate: Number(e.target.value),
                      },
                    })
                  }
                />
                kbps
              </span>
            </Row>
          </Group>

          <Group title="Recording">
            <Row label="Keep the last">
              <select
                value={draft.bufferMinutes}
                onChange={(e) => set({ bufferMinutes: Number(e.target.value) })}
                aria-label="Minutes to keep"
              >
                {BUFFER_MINUTES.map((m) => (
                  <option key={m} value={m}>
                    {m} minutes
                  </option>
                ))}
              </select>
            </Row>
            <Row label="Rewind sound">
              <Switch
                checked={draft.transition.sound}
                onChange={(sound) =>
                  set({ transition: { ...draft.transition, sound } })
                }
                label="Rewind sound"
              />
            </Row>
          </Group>
        </div>
      )}
    </dialog>
  );
}

function Group({
  title,
  note,
  children,
}: Readonly<{ title: string; note?: string; children: React.ReactNode }>) {
  return (
    <section className="group">
      <h3>{title}</h3>
      <div className="group-card">{children}</div>
      {note && <p className="group-note">{note}</p>}
    </section>
  );
}

function Row({
  label,
  children,
}: Readonly<{ label: string; children: React.ReactNode }>) {
  return (
    <div className="row">
      <span className="row-label">{label}</span>
      {children}
    </div>
  );
}

function CopyRow({ label, value }: Readonly<{ label: string; value: string }>) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="row">
      <span className="row-label">{label}</span>
      <code className="copy-value">{value}</code>
      <button
        className="text-button"
        onClick={() =>
          void navigator.clipboard.writeText(value).then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1400);
          })
        }
      >
        {copied ? 'Copied' : 'Copy'}
      </button>
    </div>
  );
}

function Switch({
  checked,
  onChange,
  label,
}: Readonly<{
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
}>) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      aria-label={label}
      className="switch"
      onClick={() => onChange(!checked)}
    >
      <span />
    </button>
  );
}

function Choice({
  options,
  value,
  onChange,
}: Readonly<{
  options: string[];
  value: string;
  onChange: (v: string) => void;
}>) {
  return (
    <div className="choice" role="radiogroup">
      {options.map((o) => (
        <button
          key={o}
          role="radio"
          aria-checked={o === value}
          onClick={() => onChange(o)}
        >
          {o}
        </button>
      ))}
    </div>
  );
}
