// THE VIDEO CHECK (VID-9): a deterministic judge of a rendered video, run by the `test` node with the runner `video_check`.
// Needs node, ffprobe and ffmpeg. The leader sets thresholds in video-check.json (never code); this file is fixed and shipped with the serve.
// It prints the same lines node's spec reporter does, so the test node reads the result the way it reads any test run.
import { execFileSync } from 'node:child_process';
import { readFileSync, existsSync } from 'node:fs';

const cfg = { video: 'out/video.mp4', duration: 5, duration_tol: 0.25, width: 1920, height: 1080,
  times: [0.5, 2.5, 4.5], min_mean_luma: 12, min_luma_std: 14, occupancy: [0.05, 0.92], centre_box: [0.12, 0.88],
  corner_share: 0.45, motion_times: [1.0, 4.0], min_motion: 1.5, min_edge: 0.6, ...JSON.parse(readFileSync(process.argv[2] || 'video-check.json', 'utf8')) };

const W = 96, H = 54;
const frame = (t) => execFileSync('ffmpeg', ['-v', 'error', '-ss', String(t), '-i', cfg.video, '-frames:v', '1', '-vf', `scale=${W}:${H},format=gray`, '-f', 'rawvideo', '-'], { maxBuffer: 1 << 24 });
const stats = (b) => { let s = 0; for (const v of b) s += v; const m = s / b.length; let q = 0; for (const v of b) q += (v - m) * (v - m); return { mean: m, std: Math.sqrt(q / b.length) }; };
const median = (a) => [...a].sort((x, y) => x - y)[a.length >> 1];
// the background: the median of the four corner patches; the foreground: what differs from it
const patch = (b, x0, y0) => { const out = []; for (let y = y0; y < y0 + 5; y++) for (let x = x0; x < x0 + 5; x++) out.push(b[y * W + x]); return out; };
const background = (b) => median([...patch(b, 0, 0), ...patch(b, W - 5, 0), ...patch(b, 0, H - 5), ...patch(b, W - 5, H - 5)]);

const results = [];
const check = (name, ok, detail) => results.push({ name, ok, detail });
try {
  if (!existsSync(cfg.video)) throw new Error(`${cfg.video} was not rendered`);
  const pr = JSON.parse(execFileSync('ffprobe', ['-v', 'error', '-show_entries', 'format=duration:stream=width,height', '-of', 'json', cfg.video]).toString());
  const dur = parseFloat(pr.format.duration), st = pr.streams.find((s) => s.width) || {};
  if (cfg.duration === null) check('the video lasts a sensible time', dur >= 3 && dur <= 40, `${dur.toFixed(2)} s, wanted 3 to 40`);
  else check('the video lasts the planned time', Math.abs(dur - cfg.duration) <= cfg.duration_tol, `${dur.toFixed(2)} s, wanted ${cfg.duration} +- ${cfg.duration_tol}`);
  check('the frame size is the planned one', st.width === cfg.width && st.height === cfg.height, `${st.width}x${st.height}, wanted ${cfg.width}x${cfg.height}`);
  const fr = cfg.times.map((t) => frame(t));
  const lum = fr.map(stats);
  check('no frame is black', lum.every((l) => l.mean >= cfg.min_mean_luma), `mean luma ${lum.map((l) => l.mean.toFixed(1)).join(', ')}, at least ${cfg.min_mean_luma}`);
  check('the frames have contrast (they are not flat)', lum.every((l) => l.std >= cfg.min_luma_std), `luma spread ${lum.map((l) => l.std.toFixed(1)).join(', ')}, at least ${cfg.min_luma_std}`);
  const mid = fr[Math.floor(fr.length / 2)], bg = background(mid);
  let n = 0, sx = 0, sy = 0, corner = 0;
  for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) if (Math.abs(mid[y * W + x] - bg) > 25) { n++; sx += x; sy += y; if (x < W * 0.3 && y < H * 0.3) corner++; }
  const occ = n / (W * H);
  check('something fills a sensible part of the frame', occ >= cfg.occupancy[0] && occ <= cfg.occupancy[1], `${(occ * 100).toFixed(1)}% of the frame differs from its background, wanted ${cfg.occupancy[0] * 100}% to ${cfg.occupancy[1] * 100}%`);
  const cx = n ? sx / n / W : 0, cy = n ? sy / n / H : 0, [lo, hi] = cfg.centre_box;
  check('the picture is centred, not pushed into a corner', n > 0 && cx >= lo && cx <= hi && cy >= lo && cy <= hi, `centre of the content at ${(cx * 100).toFixed(0)}% across, ${(cy * 100).toFixed(0)}% down, wanted inside ${lo * 100}-${hi * 100}%`);
  check('the content is not piled in one corner', n > 0 && corner / n <= cfg.corner_share, `${(n ? corner / n * 100 : 0).toFixed(0)}% of the content is in the top-left corner, at most ${cfg.corner_share * 100}%`);
  let e = 0; for (let y = 0; y < H; y++) for (let x = 1; x < W; x++) e += Math.abs(mid[y * W + x] - mid[y * W + x - 1]); e /= W * H;
  check('there is detail (edges), not a bare gradient', e >= cfg.min_edge, `edge density ${e.toFixed(2)}, at least ${cfg.min_edge}`);
  const [a, b] = cfg.motion_times.map((t) => frame(t)); let d = 0; for (let i = 0; i < a.length; i++) d += Math.abs(a[i] - b[i]); d /= a.length;
  check('something moves between the early and the late frame', d >= cfg.min_motion, `mean change ${d.toFixed(2)} between ${cfg.motion_times[0]} s and ${cfg.motion_times[1]} s, at least ${cfg.min_motion}`);
} catch (err) { check('the video can be inspected', false, String(err.message || err).split('\n')[0]); }

const bad = results.filter((r) => !r.ok);
for (const r of results) console.log(`${r.ok ? '✔' : '✖'} ${r.name} (0ms)${r.ok ? '' : `\n  ${r.detail}`}`);
if (bad.length) { console.log('✖ failing tests:'); for (const r of bad) console.log(`✖ ${r.name} (0ms)\n  ${r.detail}`); }
console.log(`ℹ tests ${results.length}\nℹ pass ${results.length - bad.length}\nℹ fail ${bad.length}`);
process.exit(bad.length ? 1 : 0);
