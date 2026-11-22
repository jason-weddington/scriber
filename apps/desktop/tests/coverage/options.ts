// One definition of what the frontend coverage gate measures, shared by the
// e2e fixture (browser coverage), the global setup and teardown, and the unit
// project (Node coverage, collected through NODE_V8_COVERAGE).
//
// Scope: everything under src/ except src/lib/mock.ts and src/main.ts. The
// mock is the test double for the Rust core, so counting it would reward
// writing more simulator code; its behaviour is pinned by its own tests
// (tests/unit/mock.spec.ts) instead. main.ts is the two-line mount.

import type { CoverageReportOptions } from 'monocart-coverage-reports';

export const COVERAGE_DIR = 'coverage';
export const NODE_V8_DIR = `${COVERAGE_DIR}/node-v8`;

/** Production web chunks served from a localhost test stand-in. */
export const WEB_CHUNK_URL = /^http:\/\/(?:localhost|127\.0\.0\.1):\d+\/assets\/[^/]+\.js(\?|$)/;

/** New web-backend modules that MUST have mapped line hits in every merged coverage run. */
export const REQUIRED_WEB_MODULES = [
  'src/lib/bridge-web.ts',
  'src/lib/browser-capture.ts',
  'src/lib/pcm16.ts',
] as const;

const IN_SCOPE = /\/src\//;
const EXCLUDED = /\/src\/(lib\/mock|main)\.ts(\?|$)/;

function inScope(url: string): boolean {
  if (WEB_CHUNK_URL.test(url)) return true;
  return IN_SCOPE.test(url) && !EXCLUDED.test(url) && !url.includes('type=style') && !url.includes('node_modules');
}

export const coverageOptions: CoverageReportOptions = {
  name: 'Frontend',
  outputDir: COVERAGE_DIR,
  reports: ['json-summary', 'console-summary', 'lcovonly'],
  entryFilter: (entry) => inScope(entry.url),
  // Browser dev sources arrive as `localhost-1430/src/lib/x.ts` (or a bare file
  // name beside their dist file), built web chunks remap via source maps to
  // `../../src/lib/x.ts` or `127.0.0.1-<port>/.../src/lib/x.ts`, and Node's as
  // `src/lib/x.ts`. Canonicalize every spelling to `src/...` so all projects
  // merge per file.
  sourcePath: (filePath, info) => {
    let path = filePath;
    if (!path.includes('/') && info.distFile) {
      path = `${info.distFile.slice(0, info.distFile.lastIndexOf('/') + 1)}${path}`;
    }
    if (path.includes('node_modules')) return path;
    path = path.replace(/^(?:localhost|127\.0\.0\.1)-\d+\//, '');
    const srcIdx = path.indexOf('/src/');
    if (srcIdx !== -1) return path.slice(srcIdx + 1);
    return path.replace(/^(?:\.\.\/)+/, '');
  },
  sourceFilter: (sourcePath) =>
    /(^|\/)src\//.test(sourcePath) &&
    !/(^|\/)src\/(lib\/mock|main)\.ts$/.test(sourcePath) &&
    !sourcePath.includes('type=style') &&
    !sourcePath.includes('node_modules'),
};
