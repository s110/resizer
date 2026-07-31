/**
 * Browser tests for the parts of the interface that only exist in the page:
 * the ffmpeg setup screen, the enlarged previews, and the A/B overlay
 * comparison. Run by CI (see .github/workflows/ci.yml):
 *
 *   node tests/ui/ui-test.mjs <path-to-resizer-binary>
 *
 * Requires ffmpeg on PATH and playwright-core with a Chromium available.
 */

import { chromium } from 'playwright-core';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import net from 'node:net';

const BIN = process.argv[2];
if (!BIN) {
  console.error('usage: node ui-test.mjs <path-to-resizer>');
  process.exit(2);
}

let failures = 0;
function check(name, cond, detail = '') {
  if (cond) {
    console.log(`  ok   ${name}`);
  } else {
    failures++;
    console.log(`  FAIL ${name}${detail ? ' — ' + detail : ''}`);
  }
}

function freePort() {
  return new Promise((resolve) => {
    const srv = net.createServer();
    srv.listen(0, '127.0.0.1', () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

async function waitForServer(port, timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/api/state`);
      if (r.ok) return true;
    } catch {}
    await new Promise((r) => setTimeout(r, 150));
  }
  throw new Error(`server on ${port} never came up`);
}

async function startServer({ withFfmpeg }) {
  const port = await freePort();
  const home = mkdtempSync(join(tmpdir(), 'resizer-ui-'));
  const env = { ...process.env, HOME: home, XDG_DATA_HOME: home, LOCALAPPDATA: home };
  if (!withFfmpeg) {
    env.PATH = '';
    env.FFMPEG_PATH = '/definitely/not/here';
  }
  const child = spawn(BIN, ['--port', String(port), '--no-browser'], {
    env,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.resume();
  child.stderr.resume();
  await waitForServer(port);
  return { child, port, home };
}

function makeMedia() {
  const dir = mkdtempSync(join(tmpdir(), 'resizer-uimedia-'));
  execFileSync('ffmpeg', [
    '-y', '-v', 'error',
    '-f', 'lavfi', '-i', 'testsrc2=size=1280x720:rate=30:duration=3',
    '-c:v', 'libx264', '-pix_fmt', 'yuv420p', join(dir, 'clip.mp4'),
  ]);
  return dir;
}

/* ---------------- test 1: ffmpeg setup screen ---------------- */
async function testSetupScreen(browser) {
  console.log('setup screen (ffmpeg missing)');
  const srv = await startServer({ withFfmpeg: false });
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  try {
    await page.goto(`http://127.0.0.1:${srv.port}/`, { waitUntil: 'networkidle' });
    await page.waitForSelector('#setup.open', { timeout: 10000 });

    check('setup screen is shown', await page.isVisible('#setup.open'));
    await page.waitForSelector('.method', { timeout: 10000 });
    const methods = await page.$$('.method');
    check('install methods are listed', methods.length >= 1, `found ${methods.length}`);

    const recommended = await page.$$('.method .badge');
    check('one method is marked recommended', recommended.length >= 1);

    const selected = await page.$$('.method.on');
    check('a method is preselected', selected.length === 1, `${selected.length} selected`);

    // The user must be able to pick a different one before installing.
    if (methods.length > 1) {
      await methods[methods.length - 1].click();
      const nowSelected = await page.$$('.method.on');
      check('choosing another method updates the selection', nowSelected.length === 1);
    }

    const detail = await page.textContent('.method .why');
    check('the method explains what it will do', (detail || '').length > 20);

    check('install button is present', await page.isVisible('#doinstall'));
    // Nothing must have been installed just by opening the screen.
    const state = await (await fetch(`http://127.0.0.1:${srv.port}/api/state`)).json();
    check('nothing installs without the user asking', state.setup.installing === false);
  } finally {
    await page.close();
    srv.child.kill();
    rmSync(srv.home, { recursive: true, force: true });
  }
}

/* ---------------- test 2: previews + A/B overlay ---------------- */
async function testPreviewAndAB(browser) {
  console.log('preview size and A/B comparison');
  const srv = await startServer({ withFfmpeg: true });
  const mediaDir = makeMedia();
  const page = await browser.newPage({ viewport: { width: 1400, height: 1000 } });
  try {
    await page.goto(`http://127.0.0.1:${srv.port}/`, { waitUntil: 'networkidle' });
    check('setup screen stays hidden when ffmpeg works', !(await page.isVisible('#setup.open')));

    await page.fill('#folderpath', mediaDir);
    await page.click('#addfolder');
    await page.waitForSelector('[data-prev]', { timeout: 20000 });

    await page.click('[data-prev]');
    await page.waitForSelector('#modal.open', { timeout: 90000 });

    // A/B is the default and puts both clips in the same box.
    check('A/B overlay is the default mode', await page.isVisible('#abbox'));
    check('side-by-side is hidden in A/B mode', !(await page.isVisible('#sidewrap')));
    const layers = await page.$$('#abbox .layer video');
    check('both versions render in the same box', layers.length === 2, `found ${layers.length}`);
    // Only one comparison is on screen at a time — no duplicated players.
    const visiblePlayers = await page.$$eval('#modal video', (vs) =>
      vs.filter((v) => v.getBoundingClientRect().height > 0).length);
    check('exactly one pair of players is visible', visiblePlayers === 2,
      `${visiblePlayers} visible`);

    // The preview must be big enough to actually judge quality.
    const box = await page.locator('#abbox').boundingBox();
    check('preview is large', box && box.height >= 400, box ? `${Math.round(box.width)}x${Math.round(box.height)}` : 'no box');

    // Dragging the handle moves the split.
    const before = await page.getAttribute('#abbox', 'data-split');
    await page.mouse.move(box.x + box.width * 0.5, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width * 0.2, box.y + box.height / 2, { steps: 8 });
    await page.mouse.up();
    const after = await page.getAttribute('#abbox', 'data-split');
    check('dragging moves the divider', parseFloat(after) < parseFloat(before) - 5,
      `${before} -> ${after}`);

    // Keyboard works too (accessibility).
    await page.locator('#abbox').focus();
    const beforeKey = parseFloat(await page.getAttribute('#abbox', 'data-split'));
    await page.keyboard.press('ArrowRight');
    const afterKey = parseFloat(await page.getAttribute('#abbox', 'data-split'));
    check('arrow keys move the divider', afterKey > beforeKey, `${beforeKey} -> ${afterKey}`);

    // Both halves must frame the same pixels, or the split compares framing
    // instead of quality.
    const fits = await page.$$eval('#abbox .layer video, #abbox .layer img', (els) =>
      els.map((e) => getComputedStyle(e).objectFit));
    check('both A/B layers share the same framing', fits.length === 2 && fits.every((f) => f === 'cover'),
      fits.join(', '));

    // The clip-path actually follows the split, i.e. the overlay really clips.
    const clip = await page.evaluate(() =>
      getComputedStyle(document.querySelector('#abbox .layer.after')).clipPath);
    check('the "after" layer is clipped to the split', /inset\(/.test(clip), clip);

    // Side-by-side mode still available.
    await page.click('#modeside');
    check('side-by-side mode shows two figures', await page.isVisible('#sidewrap'));
    check('A/B is hidden in side-by-side mode', !(await page.isVisible('#abbox')));
    const sideBox = await page.locator('#sidewrap video').first().boundingBox();
    check('side-by-side previews are large', sideBox && sideBox.width >= 350,
      sideBox ? `${Math.round(sideBox.width)}px wide` : 'no box');

    await page.click('#modeab');
    check('switching back to A/B works', await page.isVisible('#abbox'));

    // Closing stops playback (no orphaned decoders).
    await page.click('#closemodal');
    const leftovers = await page.$$('#abbox video');
    check('closing clears the players', leftovers.length === 0);
  } finally {
    await page.close();
    srv.child.kill();
    rmSync(srv.home, { recursive: true, force: true });
    rmSync(mediaDir, { recursive: true, force: true });
  }
}

const browser = await chromium.launch({
  executablePath: process.env.CHROMIUM_PATH || undefined,
});
try {
  await testSetupScreen(browser);
  await testPreviewAndAB(browser);
} finally {
  await browser.close();
}

console.log(failures === 0 ? '\nall UI checks passed' : `\n${failures} UI check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
