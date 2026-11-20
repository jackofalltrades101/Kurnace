// Phase C exit session (docs/v2-plan.md §4): a person edits while an agent
// edits concurrently. Asserts: edits show at once and never snap back, a
// rejected edit rolls back with a reason, a library drop lands where it was
// dropped, and at the end the UI and the core agree with nothing lost.
import { spawn } from 'node:child_process';

const fail = msg => { throw new Error(msg); };
/** Run one step with a deadline so a hang names the step instead of stalling the run. */
const step = async (name, fn, ms = 20000) => {
  console.log(`-- ${name}`);
  let t;
  try {
    return await Promise.race([fn(), new Promise((_, rej) => { t = setTimeout(() => rej(new Error(`step "${name}" timed out after ${ms / 1000}s`)), ms); })]);
  } finally { clearTimeout(t); }
};

export default async (page, { sleep, kurn, project }) => {
  const view = () => page.evaluate(() => window.__kurnView);
  const pt = (f, track) => page.evaluate((f, t) => window.__kurnTimelineUI.point(f, t), f, track);
  /** Sample the UI every animation frame for `ms`; return every value of fn(view). */
  const watch = (fnSrc, ms) => page.evaluate((src, ms) => new Promise(res => {
    const fn = new Function('v', `return (${src})(v)`);
    const seen = []; const t0 = performance.now();
    const tick = () => { seen.push(fn(window.__kurnView)); performance.now() - t0 < ms ? requestAnimationFrame(tick) : res(seen); };
    tick();
  }), fnSrc, ms);

  // The agent works the whole time: 15 markers, one by one.
  const agent = spawn('sh', ['-c', `for i in $(seq 1 15); do "${process.env.KURN_BIN}" --project "${project}" op marker.add --args "{\\"t\\":$((i * 7)),\\"label\\":\\"agent $i\\"}" >/dev/null; done`],
    { env: { ...process.env, KURN_ACTOR: 'agent:claude' }, stdio: 'ignore' });
  const agentDone = new Promise(r => agent.on('close', r));

  await step('drag c1 +100 frames', async () => {
  // 1. Drag c1 (0–60 on v1) right by 100 frames with the real mouse.
  const ppf = await page.evaluate(() => window.__kurnTimelineUI.pxPerFrame);
  const a = await pt(30, 'v1');
  await page.mouse.move(a.x, a.y);
  await page.keyboard.down('Alt'); // no snapping: land exactly where dropped
  await page.mouse.down();
  for (let i = 1; i <= 10; i++) await page.mouse.move(a.x + (100 * ppf * i) / 10, a.y);
  await page.mouse.up();
  await page.keyboard.up('Alt');
  const starts = await watch('v => v.clips.c1.start', 1000);
  console.log(`   drag: c1.start over 1 s after drop = ${[...new Set(starts)].join(' → ')}`);
  if (Math.abs(starts[0] - 100) > 2) fail(`drop didn't show immediately (first frame: ${starts[0]})`);
  if (starts.some(s => s !== starts[0])) fail(`snap-back/jump after drop: ${[...new Set(starts)]}`);

  });
  let field;
  await step('inspector: c2 start → 300', async () => {
  // 2. Inspector: select c2 and type a new start.
  const b = await pt(230, 'v1');
  await page.mouse.click(b.x, b.y);
  await sleep(300);
  field = await page.$('input[aria-label="Start"]');
  if (!field) fail('inspector did not open for c2');
  await field.click(); await field.evaluate(el => el.select());
  await page.keyboard.type('300');
  await page.keyboard.press('Enter');
  const s2 = await watch('v => v.clips.c2.start', 800);
  console.log(`   inspector: c2.start = ${[...new Set(s2)].join(' → ')}`);
  if (s2[0] !== 300 || s2.some(s => s !== 300)) fail('inspector edit did not apply immediately and stick');

  });
  await step('rejected edit rolls back', async () => {
  // 3. A rejected edit rolls back and explains: move c2 onto c1 (100–160).
  field = await page.$('input[aria-label="Start"]');
  await field.click(); await field.evaluate(el => el.select());
  await page.keyboard.type('120');
  await page.keyboard.press('Enter');
  const s3 = await watch('v => v.clips.c2.start', 1200);
  const toast = await page.evaluate(() => document.querySelector('.toast')?.textContent ?? '');
  console.log(`   rejected: c2.start = ${[...new Set(s3)].join(' → ')} · toast: ${toast.slice(0, 80)}`);
  if (s3[s3.length - 1] !== 300) fail('rejected edit did not roll back');
  if (!/overlap/i.test(toast)) fail('no explanation shown for the rejected edit');

  });
  await step('drop scene from library', async () => {
  // 4. Drop the "intro" scene from the library onto v2 at frame 200.
  const d = await pt(200, 'v2');
  await page.evaluate(({ x, y }) => {
    const dt = new DataTransfer();
    dt.setData('application/x-kurn-scene', 'intro');
    const wrap = document.querySelector('.tl-wrap');
    for (const type of ['dragover', 'drop']) wrap.dispatchEvent(new DragEvent(type, { bubbles: true, cancelable: true, clientX: x, clientY: y, dataTransfer: dt }));
  }, d);
  await sleep(600);
  const dropped = Object.entries((await view()).clips).find(([, c]) => c.scene === 'intro');
  console.log(`   drop: ${dropped ? `${dropped[0]} on ${dropped[1].track} at ${dropped[1].start}` : 'nothing'}`);
  if (!dropped || dropped[1].track !== 'v2' || Math.abs(dropped[1].start - 200) > 2) fail('scene did not land where it was dropped');

  });
  await step('UI and core agree', async () => {
  // 5. Settle, then the UI and the core must agree, with the agent's work intact.
  await agentDone;
  await sleep(1200);
  const ui = await view();
  const core = kurn('timeline', 'show').timeline;
  const shape = t => JSON.stringify(Object.values(t.clips).map(c => [c.track, c.start, c.out - c.in, c.scene ?? c.component ?? c.asset]).sort());
  console.log(`   end: ${Object.keys(core.clips).length} clips, ${Object.keys(core.markers).length} markers in core`);
  if (shape(ui) !== shape(core)) fail(`UI and core disagree:\n ui   ${shape(ui)}\n core ${shape(core)}`);
  if (Object.keys(core.markers).length !== 15) fail(`agent markers lost: ${Object.keys(core.markers).length}/15`);
  if (Object.keys(ui.markers).length !== 15) fail('UI is missing agent markers');
  if (core.clips.c1.start !== ui.clips.c1.start || core.clips.c2.start !== 300) fail('human edits lost in the core');
  }, 60000);
};
