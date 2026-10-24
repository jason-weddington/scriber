import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

// The dev port is a pinned literal and strictPort is mandatory: without it
// Vite silently hops to the next port when 1420 is taken, and Tauri's devUrl
// (src-tauri/tauri.conf.json) then loads nothing. Playwright runs its own
// server on 1430 (playwright.config.ts).
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
});
