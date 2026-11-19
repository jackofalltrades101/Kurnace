/** @jsxImportSource react */
// Library panel (spec §8, §15): search, kind filter, previews, drag-and-drop import.
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { useEffect, useState } from 'react';
import { type Asset, assetUrl, call } from './kurn';

const KINDS = ['all', 'image', 'video', 'audio', 'seq', 'font'] as const;

function Thumb({ a }: { a: Asset }) {
  const url = assetUrl(a.id, a.ext);
  if (a.kind === 'image') return <img src={url} alt="" loading="lazy" />;
  if (a.kind === 'video') return <video src={`${url}#t=0.5`} preload="metadata" muted />;
  return <div className="glyph" aria-hidden>{a.kind === 'audio' ? '♪' : a.kind === 'seq' ? '▦' : '◇'}</div>;
}

function meta(a: Asset) {
  const bits = [];
  if (a.width && a.height) bits.push(`${a.width}×${a.height}`);
  if (a.duration_s) bits.push(`${a.duration_s.toFixed(1)}s`);
  if (a.source === 'generated') bits.push('generated');
  return bits.join(' · ');
}

export function Library({ refreshKey }: { refreshKey: number }) {
  const [q, setQ] = useState('');
  const [kind, setKind] = useState<(typeof KINDS)[number]>('all');
  const [semantic, setSemantic] = useState(false);
  const [items, setItems] = useState<Asset[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [dropping, setDropping] = useState(false);
  const [bump, setBump] = useState(0);

  useEffect(() => {
    const t = setTimeout(() => {
      call<Asset[]>('lib.search', { query: q, kind: kind === 'all' ? null : kind, semantic, limit: 300 })
        .then(setItems).catch(e => setBusy(String(e)));
    }, 120);
    return () => clearTimeout(t);
  }, [q, kind, semantic, refreshKey, bump]);

  // Drop files from Finder onto the window → library.
  useEffect(() => {
    const un = getCurrentWebview().onDragDropEvent(async e => {
      if (e.payload.type === 'over' || e.payload.type === 'enter') setDropping(true);
      else if (e.payload.type === 'leave') setDropping(false);
      else if (e.payload.type === 'drop') {
        setDropping(false);
        setBusy(`importing ${e.payload.paths.length} file(s)…`);
        try {
          await call('lib.add', { paths: e.payload.paths, tags: [] });
          setBusy(null);
          setBump(b => b + 1);
        } catch (err) {
          setBusy(String(err));
        }
      }
    });
    return () => { un.then(f => f()); };
  }, []);

  return (
    <section className={`panel library${dropping ? ' dropping' : ''}`} aria-label="Library">
      <header>
        <h2>Library</h2>
        <span className="muted">{items.length}</span>
      </header>
      <div className="lib-controls">
        <input type="search" placeholder="Search name or tag" value={q} onChange={e => setQ(e.target.value)} />
        <div className="chips" role="radiogroup" aria-label="Kind">
          {KINDS.map(k => (
            <button key={k} role="radio" aria-checked={kind === k} className={kind === k ? 'on' : ''} onClick={() => setKind(k)}>{k}</button>
          ))}
        </div>
        <div className="chips" role="group" aria-label="Search mode">
          <button role="switch" aria-checked={semantic} className={semantic ? 'on' : ''} onClick={() => setSemantic(s => !s)} title="Local similarity search (Ollama, or offline n-gram fallback)">semantic</button>
        </div>
      </div>
      {busy && <div className="note">{busy}</div>}
      <ul className="assets">
        {items.map(a => (
          <li key={a.id} title={`${a.id}\n${a.tags.join(', ')}`}
              draggable onDragStart={e => e.dataTransfer.setData('text/plain', a.id)}>
            <div className="thumb"><Thumb a={a} /></div>
            <div className="name">{a.name ?? a.id}</div>
            <div className="meta muted">{meta(a)}</div>
            <button className="copy" onClick={() => navigator.clipboard.writeText(a.id)} aria-label={`Copy id of ${a.name}`}>id</button>
          </li>
        ))}
        {items.length === 0 && <li className="empty muted">Nothing here. Drop files onto the window, or <code>kurn lib add</code>.</li>}
      </ul>
    </section>
  );
}
