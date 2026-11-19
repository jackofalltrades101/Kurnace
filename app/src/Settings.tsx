/** @jsxImportSource react */
// Settings dialog. The Fish key lives in the OS keychain; defaults live in
// ~/.kurnace/settings.json and are injected into agent panes and module runs.
import { useEffect, useRef, useState } from 'react';
import { type Asset, assetUrl, call, fishClear, fishSave, fishStatus, onJob } from './kurn';

export function Settings({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [configured, setConfigured] = useState(false);
  const [hint, setHint] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [voice, setVoice] = useState('');
  const [model, setModel] = useState('s2.1-pro');
  const [speed, setSpeed] = useState(1);
  const [text, setText] = useState('Kurnace turns code into video.');
  const [busy, setBusy] = useState<string | null>(null);
  const [pct, setPct] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [clip, setClip] = useState<Asset | null>(null);
  const busyRef = useRef(false);
  busyRef.current = !!busy;

  const load = () => {
    fishStatus().then(s => {
      setConfigured(s.configured);
      setHint(s.hint ?? '');
      setVoice(s.voice ?? '');
      setModel(s.model || 's2.1-pro');
      setSpeed(s.speed || 1);
    }).catch(e => setError(String(e)));
  };
  useEffect(() => { if (open) { setError(null); setSaved(false); load(); } }, [open]);
  useEffect(() => {
    let un: (() => void) | undefined;
    onJob(e => {
      if (!busyRef.current) return;
      if (e.event.type === 'progress') setPct(e.event.pct ?? 0);
    }).then(u => (un = u));
    return () => un?.();
  }, []);
  if (!open) return null;

  const save = async () => {
    setBusy('save');
    setError(null);
    setSaved(false);
    try {
      const s = await fishSave({ apiKey: apiKey || undefined, voice, model, speed });
      setConfigured(s.configured);
      setHint(s.hint ?? '');
      setApiKey('');
      setSaved(true);
    } catch (e) {
      setError(String(e).replace(/^Error: /, ''));
    } finally { setBusy(null); }
  };

  const remove = async () => {
    setBusy('remove');
    setError(null);
    try {
      const s = await fishClear();
      setConfigured(s.configured);
      setHint('');
      setSaved(false);
    } catch (e) { setError(String(e)); } finally { setBusy(null); }
  };

  const generate = async () => {
    setBusy('generate');
    setPct(0);
    setError(null);
    setClip(null);
    busyRef.current = true;
    try {
      const r = await call<{ assets?: { audio?: Asset } }>('run', {
        module: 'tts.fish',
        params: { text, voice, model, speed },
      });
      setClip(r.assets?.audio ?? null);
    } catch (e) {
      setError(String(e).replace(/^Error: /, ''));
    } finally {
      busyRef.current = false;
      setBusy(null);
    }
  };

  return (
    <div className="settings-overlay" onMouseDown={e => { if (e.target === e.currentTarget) onClose(); }}>
      <div className="settings" role="dialog" aria-modal="true" aria-label="Settings">
        <header>
          <h2>Settings</h2>
          <button className="x" onClick={onClose} aria-label="Close settings">×</button>
        </header>

        <section>
          <h3>Fish Audio</h3>
          <p className="muted small">
            The API key is stored in the OS keychain and injected into module runs and agent panes. It never appears in a prompt or project file.
          </p>
          <div className="fish-status">
            <span className={configured ? 'ok' : 'muted'}>{configured ? `key configured ${hint}` : 'no key configured'}</span>
            {configured && <button onClick={() => void remove()} disabled={!!busy}>Remove key</button>}
          </div>
          <div className="form-grid">
            <label>API key
              <input type="password" value={apiKey} onChange={e => setApiKey(e.target.value)} placeholder={configured ? 'leave blank to keep current key' : 'fish_…'} autoComplete="off" />
            </label>
            <label>Default voice id
              <input value={voice} onChange={e => setVoice(e.target.value)} placeholder="Fish reference_id (optional)" />
            </label>
            <label>Model
              <input value={model} onChange={e => setModel(e.target.value)} placeholder="s2.1-pro" />
            </label>
            <label>Speed
              <input type="number" min={0.5} max={2} step={0.05} value={speed} onChange={e => setSpeed(Number(e.target.value))} />
            </label>
          </div>
          <div className="row">
            <button className="primary" onClick={() => void save()} disabled={!!busy}>{configured ? 'Save defaults' : 'Save Fish key'}</button>
            {saved && <span className="ok">saved</span>}
          </div>
        </section>

        <section>
          <h3>Test / generate voiceover</h3>
          <p className="muted small">Runs <code>tts.fish</code> through the core and adds the result to the library.</p>
          <textarea value={text} onChange={e => setText(e.target.value)} rows={3} aria-label="Voiceover text" />
          <div className="row">
            <button className="primary" onClick={() => void generate()} disabled={!!busy || !text.trim()}>
              {busy === 'generate' ? `Generating${pct ? ` ${Math.round(pct)}%` : '…'}` : 'Generate voiceover'}
            </button>
            {!configured && <span className="muted small">save a key first</span>}
          </div>
          {clip && (
            <div className="fish-result">
              <audio controls src={assetUrl(clip.id, clip.ext)} />
              <code>{clip.id}</code>
            </div>
          )}
        </section>

        {error && <div className="note err" role="alert">{error}</div>}
      </div>
    </div>
  );
}
