import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { extname, join, resolve } from 'node:path';

import { build } from 'vite';

import { expect, test } from './fixtures';

import { chooseButton } from './helpers';

// The built app (what Tauri serves) under the exact production CSP from
// tauri.conf.json, on the browser mock: any inline script or style, eval or
// stray origin in the bundle shows up here as a violation instead of as a
// blank window in the packaged app.

const CONF = resolve(process.cwd(), 'src-tauri/tauri.conf.json');
const ORIGIN = 'http://csp.test';
const TYPES: Record<string, string> = {
  '.html': 'text/html',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.svg': 'image/svg+xml',
};

let dist = '';

test.beforeAll(async () => {
  dist = mkdtempSync(join(tmpdir(), 'csp-'));
  await build({ logLevel: 'silent', build: { outDir: dist, emptyOutDir: true } });
});

test.afterAll(() => {
  rmSync(dist, { recursive: true, force: true });
});

test('the built app runs under the production CSP with no violations', async ({ page }) => {
  const conf = JSON.parse(readFileSync(CONF, 'utf8')) as { app: { security: { csp: string } } };
  const csp = conf.app.security.csp;
  await page.route(`${ORIGIN}/**`, async (route) => {
    const path = new URL(route.request().url()).pathname;
    const file = join(dist, path === '/' ? 'index.html' : path);
    await route.fulfill({
      body: readFileSync(file),
      contentType: TYPES[extname(file)] ?? 'application/octet-stream',
      headers: { 'Content-Security-Policy': csp },
    });
  });
  const violations: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error' && /Content Security Policy/i.test(m.text())) violations.push(m.text());
  });
  await page.addInitScript(() => {
    document.addEventListener('securitypolicyviolation', (e) => {
      console.error(`Content Security Policy violation: ${e.violatedDirective} ${e.blockedURI}`);
    });
  });
  await page.goto(`${ORIGIN}/`);
  await expect(page.locator('html')).toHaveAttribute('data-bridge', 'mock');
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
  // The summary sets bar widths at run time; that must not need inline styles.
  await chooseButton(page).click();
  const fill = page.locator('.fill').first();
  await expect(fill).toBeVisible();
  expect(await fill.evaluate((el) => el.style.width)).toBe('100%');
  expect(violations).toEqual([]);
});
