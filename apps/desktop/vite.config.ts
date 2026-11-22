import { resolve } from 'node:path';

import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig, searchForWorkspaceRoot, type Plugin } from 'vite';

const isWeb = process.env.VITE_SCRIBER_BACKEND === 'web';
const emitTestMaps = isWeb && process.env.SCRIBER_WEB_SOURCEMAPS === '1';
const VIEWPORT_META = /<meta\s+name="viewport"\s+content="[^"]*"\s*\/>/;

/**
 * Updates the viewport meta tag and injects standalone PWA install metadata
 * only for the web build (`VITE_SCRIBER_BACKEND=web`); desktop/mock HTML stays untouched.
 */
function webPwaHtml(): Plugin {
  return {
    name: 'scriber-web-pwa-html',
    transformIndexHtml(html) {
      if (!isWeb) return html;
      if (!VIEWPORT_META.test(html)) {
        throw new Error('index.html is missing the viewport meta tag.');
      }
      return {
        html: html.replace(
          VIEWPORT_META,
          '<meta name="viewport" content="width=device-width, initial-scale=1.0, user-scalable=no" />',
        ),
        tags: [
          { tag: 'meta', attrs: { name: 'apple-mobile-web-app-capable', content: 'yes' }, injectTo: 'head' },
          { tag: 'meta', attrs: { name: 'apple-mobile-web-app-title', content: 'Scriber' }, injectTo: 'head' },
          { tag: 'link', attrs: { rel: 'apple-touch-icon', href: '/apple-touch-icon.png' }, injectTo: 'head' },
          { tag: 'link', attrs: { rel: 'manifest', href: '/manifest.webmanifest' }, injectTo: 'head' },
        ],
      };
    },
  };
}

/**
 * Verifies the built web bundle's emitted JS module graph is nonempty and
 * never pulls in the development mock (`src/lib/mock.ts`).
 */
function verifyWebBundle(): Plugin {
  return {
    name: 'scriber-verify-web-bundle',
    apply: 'build',
    generateBundle(_options, bundle) {
      if (!isWeb) return;
      const modules: string[] = [];
      for (const item of Object.values(bundle)) {
        if (item.type === 'chunk') {
          modules.push(...Object.keys(item.modules));
        }
      }
      if (modules.length === 0) {
        this.error('Web build emitted an empty JavaScript module graph.');
      }
      const leakedMock = modules.find((id) => /(^|[\\/])src[\\/]lib[\\/]mock\.ts(\?|$)/.test(id));
      if (leakedMock !== undefined) {
        this.error(`Web build unexpectedly bundled the mock module: ${leakedMock}`);
      }
      const leakedDict = modules.find((id) => /(^|[\\/])scowl-en-60\.txt(\?|$)/.test(id));
      if (leakedDict !== undefined) {
        this.error(`Web build unexpectedly bundled the mock dictionary asset: ${leakedDict}`);
      }
    },
  };
}

// The dev port is a pinned literal and strictPort is mandatory: without it
// Vite silently hops to the next port when 1420 is taken, and Tauri's devUrl
// (src-tauri/tauri.conf.json) then loads nothing. Playwright runs its own
// server on 1430 (playwright.config.ts).
export default defineConfig({
  plugins: [svelte(), webPwaHtml(), verifyWebBundle()],
  clearScreen: false,
  build: {
    outDir: isWeb ? 'dist-web' : 'dist',
    sourcemap: emitTestMaps,
    minify: emitTestMaps ? false : 'oxc',
  },
  server: {
    port: 1420,
    strictPort: true,
    fs: {
      allow: [
        searchForWorkspaceRoot(process.cwd()),
        resolve(import.meta.dirname, '../../crates/scriber-engine/data'),
      ],
    },
  },
});
