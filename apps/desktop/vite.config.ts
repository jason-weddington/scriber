import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig, type Plugin } from 'vite';

const isWeb = process.env.VITE_SCRIBER_BACKEND === 'web';
const emitTestMaps = isWeb && process.env.SCRIBER_WEB_SOURCEMAPS === '1';

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
    },
  };
}

// The dev port is a pinned literal and strictPort is mandatory: without it
// Vite silently hops to the next port when 1420 is taken, and Tauri's devUrl
// (src-tauri/tauri.conf.json) then loads nothing. Playwright runs its own
// server on 1430 (playwright.config.ts).
export default defineConfig({
  plugins: [svelte(), verifyWebBundle()],
  clearScreen: false,
  build: {
    outDir: isWeb ? 'dist-web' : 'dist',
    sourcemap: emitTestMaps,
    minify: emitTestMaps ? false : 'oxc',
  },
  server: {
    port: 1420,
    strictPort: true,
  },
});
