// three.render (spec §12): a declarative three.js scene → PNG frame sequence.
// Runs three.js inside headless Chromium (the same browser family the final
// renderer uses), then lets the core ingest the directory as a `seq` asset.
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import puppeteer from 'puppeteer';

const here = path.dirname(fileURLToPath(import.meta.url));
const threeDir = path.join(here, 'node_modules', 'three', 'build');

type Ev = Record<string, unknown> & { type: string };
const emit = (e: Ev) => process.stdout.write(JSON.stringify(e) + '\n');

interface Request {
  scene?: unknown;
  file?: string;
  frames?: number;
  fps?: number;
  width?: number;
  height?: number;
  _ctx: { work_dir: string; project_root?: string | null; seed?: number | null };
}

const HTML = `<!doctype html>
<html><head><meta charset="utf-8"><style>
html,body{margin:0;background:#000}canvas{display:block}
</style></head><body><canvas id="c"></canvas>
<script type="module">
import * as THREE from './three.module.min.js';
window.THREE = THREE;
let renderer, scene, camera, animated = [];
const vec = (v, d) => Array.isArray(v) ? v : d;

window.setup = (desc, width, height) => {
  const canvas = document.getElementById('c');
  renderer = new THREE.WebGLRenderer({ canvas, antialias: true, preserveDrawingBuffer: true });
  renderer.setPixelRatio(1);
  renderer.setSize(width, height, false);
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  scene = new THREE.Scene();
  scene.background = new THREE.Color(desc.background || '#0b0c0d');

  const cam = desc.camera || {};
  const fov = cam.fov ?? 45;
  camera = new THREE.PerspectiveCamera(fov, width / height, 0.1, 1000);
  camera.position.fromArray(vec(cam.position, [3, 2, 4]));
  camera.lookAt(new THREE.Vector3().fromArray(vec(cam.lookAt, [0, 0, 0])));

  for (const l of (desc.lights || [{ type: 'directional', position: [3, 5, 2], intensity: 2.2 }])) {
    let light;
    if (l.type === 'ambient') light = new THREE.AmbientLight(l.color || '#ffffff', l.intensity ?? 1);
    else if (l.type === 'point') {
      light = new THREE.PointLight(l.color || '#ffffff', l.intensity ?? 10, 0, 2);
      light.position.fromArray(vec(l.position, [2, 3, 2]));
    } else if (l.type === 'hemisphere') {
      light = new THREE.HemisphereLight(l.color || '#ffffff', l.groundColor || '#111111', l.intensity ?? 1);
    } else {
      light = new THREE.DirectionalLight(l.color || '#ffffff', l.intensity ?? 2);
      light.position.fromArray(vec(l.position, [3, 5, 2]));
    }
    scene.add(light);
  }

  animated = [];
  for (const o of (desc.objects || [])) {
    const geo = geometry(o);
    const mat = new THREE.MeshStandardMaterial({
      color: o.color || '#4f8cff',
      roughness: o.roughness ?? 0.4,
      metalness: o.metalness ?? 0.15,
      emissive: o.emissive || '#000000',
      emissiveIntensity: o.emissiveIntensity ?? 0,
      side: o.side === 'double' ? THREE.DoubleSide : THREE.FrontSide,
    });
    const mesh = new THREE.Mesh(geo, mat);
    mesh.position.fromArray(vec(o.position, [0, 0, 0]));
    const base = vec(o.rotation, [0, 0, 0]);
    mesh.rotation.set(base[0], base[1], base[2]);
    if (o.scale != null) {
      if (Array.isArray(o.scale)) mesh.scale.fromArray(o.scale);
      else mesh.scale.setScalar(o.scale);
    }
    scene.add(mesh);
    animated.push({ mesh, base, speed: vec(o.rotationSpeed, [0, 0.06, 0]) });
  }
};

function geometry(o) {
  const s = o.size || [1, 1, 1];
  switch (o.type) {
    case 'sphere': return new THREE.SphereGeometry(o.radius ?? 0.8, 40, 28);
    case 'torus': return new THREE.TorusGeometry(o.radius ?? 0.8, o.tube ?? 0.25, 20, 48);
    case 'cylinder': return new THREE.CylinderGeometry(o.radiusTop ?? 0.6, o.radiusBottom ?? 0.6, o.height ?? 1.4, 32);
    case 'plane': return new THREE.PlaneGeometry(s[0] ?? 10, s[1] ?? 10);
    case 'cone': return new THREE.ConeGeometry(o.radius ?? 0.7, o.height ?? 1.4, 32);
    default: return new THREE.BoxGeometry(s[0] ?? 1, s[1] ?? 1, s[2] ?? 1);
  }
}

window.renderFrame = (frame) => {
  for (const a of animated) {
    a.mesh.rotation.set(
      a.base[0] + a.speed[0] * frame,
      a.base[1] + a.speed[1] * frame,
      a.base[2] + a.speed[2] * frame,
    );
  }
  renderer.render(scene, camera);
  return renderer.domElement.toDataURL('image/png');
};
window.__ready = true;
</script></body></html>`;

async function inputScene(req: Request): Promise<unknown> {
  if (req.scene && typeof req.scene === 'object') return req.scene;
  if (req.file) {
    const root = req._ctx.project_root || process.cwd();
    const p = path.resolve(root, req.file);
    return JSON.parse(await readFile(p, 'utf8'));
  }
  throw new Error('three.render needs --scene <json> or --file <scene.json>');
}

async function main() {
  const req = JSON.parse(await new Promise<string>((resolve) => {
    const chunks: Buffer[] = [];
    process.stdin.on('data', (c) => chunks.push(c as Buffer));
    process.stdin.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
  })) as Request;
  const frames = Math.max(1, Math.round(req.frames ?? 60));
  const fps = Math.max(1, Math.round(req.fps ?? 30));
  const width = Math.max(16, Math.round(req.width ?? 640));
  const height = Math.max(16, Math.round(req.height ?? 360));
  const desc = await inputScene(req);
  const out = path.join(req._ctx.work_dir, 'frames');
  await mkdir(out, { recursive: true });

  const server = createServer((rq, rs) => {
    const url = (rq.url || '/').split('?')[0];
    if (url === '/' || url === '/index.html') {
      rs.writeHead(200, { 'Content-Type': 'text/html' });
      rs.end(HTML);
    } else if (url === '/three.module.min.js' || url === '/three.core.min.js') {
      rs.writeHead(200, { 'Content-Type': 'text/javascript' });
      rs.end(readFileSync(path.join(threeDir, url.slice(1))));
    } else {
      rs.writeHead(404).end();
    }
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = (server.address() as { port: number }).port;
  const browser = await puppeteer.launch({
    headless: true,
    args: ['--no-sandbox', '--enable-unsafe-swiftshader'],
  });
  try {
    const page = await browser.newPage();
    await page.setViewport({ width, height, deviceScaleFactor: 1 });
    page.on('pageerror', (e) => emit({ type: 'log', level: 'error', msg: `page error: ${e.message}` }));
    await page.goto(`http://127.0.0.1:${port}/`, { waitUntil: 'load' });
    await page.waitForFunction('window.__ready === true', { timeout: 20_000 });
    await page.evaluate((d, w, h) => window['setup'](d, w, h), desc, width, height);

    for (let f = 0; f < frames; f++) {
      const data = await page.evaluate((i) => window['renderFrame'](i), f) as string;
      const png = Buffer.from(data.replace(/^data:image\/png;base64,/, ''), 'base64');
      await writeFile(path.join(out, `frame${String(f).padStart(8, '0')}.png`), png);
      if (f === 0 || (f + 1) % 10 === 0 || f + 1 === frames) {
        emit({ type: 'progress', pct: Math.round(5 + (f + 1) / frames * 90), msg: `frame ${f + 1}/${frames}` });
      }
    }

    const meta = {
      engine: 'three',
      engine_version: 'three.js 0.180.0',
      language: 'javascript',
      fps_num: fps,
      fps_den: 1,
      frames,
      width,
      height,
    };
    await writeFile(path.join(out, 'kurn.json'), JSON.stringify(meta, null, 2) + '\n');
    emit({ type: 'output', name: 'frames', path: out });
    emit({ type: 'done' });
  } finally {
    await browser.close().catch(() => {});
    server.close();
  }
}

main().then(() => process.exit(0), (e) => {
  emit({ type: 'error', msg: e instanceof Error ? e.message : String(e) });
  process.exit(1);
});
