import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { expect, test } from '@playwright/test';

// The webview's Content Security Policy: no script from anywhere but the
// app's bundle, nothing evaluated from a string, no plugins, and Tauri's IPC
// as the only place to connect. tests/e2e/csp.spec.ts runs the built app
// under the same string.

const CONF = resolve(process.cwd(), 'src-tauri/tauri.conf.json');

function directives(csp: string): Map<string, string[]> {
  return new Map(
    csp
      .split(';')
      .map((d) => d.trim().split(/\s+/))
      .filter((d) => d[0] !== '')
      .map(([name = '', ...values]) => [name, values]),
  );
}

function security(): { csp: unknown; devCsp: unknown } {
  const conf = JSON.parse(readFileSync(CONF, 'utf8')) as { app: { security: { csp: unknown; devCsp: unknown } } };
  return conf.app.security;
}

test('the production CSP is set and strict', () => {
  const csp = security().csp;
  expect(typeof csp).toBe('string');
  const d = directives(csp as string);
  expect(d.get('default-src')).toEqual(["'self'"]);
  expect(d.get('script-src')).toEqual(["'self'"]);
  expect(d.get('style-src')).toEqual(["'self'"]);
  expect(d.get('object-src')).toEqual(["'none'"]);
  expect(d.get('base-uri')).toEqual(["'none'"]);
  expect(d.get('form-action')).toEqual(["'none'"]);
  expect(d.get('connect-src')).toEqual(["'self'", 'ipc:', 'http://ipc.localhost']);
  expect(csp as string).not.toContain("'unsafe-eval'");
  expect(csp as string).not.toContain("'unsafe-inline'");
  expect(csp as string).not.toContain('*');
});

test('the dev CSP differs only where Vite needs it', () => {
  const dev = security().devCsp;
  expect(typeof dev).toBe('string');
  expect(dev as string).not.toContain("'unsafe-eval'");
  const d = directives(dev as string);
  expect(d.get('script-src')).toEqual(["'self'"]);
  // Vite's dev server injects CSS as <style> elements and talks HMR over a socket.
  expect(d.get('style-src')).toEqual(["'self'", "'unsafe-inline'"]);
  expect(d.get('connect-src')).toContain('ws://localhost:1420');
});
