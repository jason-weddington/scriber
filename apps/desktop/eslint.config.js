// The frontend lint bar: typescript-eslint's strict type-checked rules, the
// svelte plugin's recommended set, and the architecture rule that only
// src/lib/bridge.ts may import Tauri. Run with --max-warnings 0, so a warning
// fails the gate exactly like an error. scripts/selftest-checks.sh proves the
// Tauri import ban is wired.
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import { defineConfig } from 'eslint/config';
import tseslint from 'typescript-eslint';

const TAURI_BAN = {
  patterns: [
    {
      group: ['@tauri-apps/*'],
      message: 'Only src/lib/bridge.ts imports Tauri; call the bridge function instead.',
    },
  ],
};

const parserOptions = {
  projectService: { allowDefaultProject: ['*.js'] },
  tsconfigRootDir: import.meta.dirname,
  extraFileExtensions: ['.svelte'],
};

export default defineConfig(
  {
    ignores: ['dist/', 'target/', 'coverage/', 'test-results/', 'playwright-report/', 'src-tauri/', '.svelte-check/'],
  },
  ...tseslint.configs.strictTypeChecked,
  ...svelte.configs['flat/recommended'],
  {
    languageOptions: {
      parserOptions,
      globals: { ...globals.browser, ...globals.node },
    },
    rules: {
      'no-restricted-imports': ['error', TAURI_BAN],
      // Numbers in template literals are fine (counts, sizes); objects are not.
      '@typescript-eslint/restrict-template-expressions': ['error', { allowNumber: true }],
    },
  },
  {
    files: ['**/*.svelte', '**/*.svelte.ts'],
    languageOptions: {
      parserOptions: { ...parserOptions, parser: tseslint.parser },
    },
  },
  {
    // The one file that talks to Tauri.
    files: ['src/lib/bridge.ts'],
    rules: { 'no-restricted-imports': 'off' },
  },
);
