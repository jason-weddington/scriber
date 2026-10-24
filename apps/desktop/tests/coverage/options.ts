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

const IN_SCOPE = /\/src\//;
const EXCLUDED = /\/src\/(lib\/mock|main)\.ts(\?|$)/;

function inScope(url: string): boolean {
  return IN_SCOPE.test(url) && !EXCLUDED.test(url) && !url.includes('type=style') && !url.includes('node_modules');
}

export const coverageOptions: CoverageReportOptions = {
  name: 'Frontend',
  outputDir: COVERAGE_DIR,
  reports: ['json-summary', 'console-summary', 'lcovonly'],
  entryFilter: (entry) => inScope(entry.url),
  // Browser sources arrive as `localhost-1430/src/lib/x.ts` (or a bare file
  // name beside their dist file); Node's as `src/lib/x.ts`. One spelling, so
  // both runs merge per file.
  sourcePath: (filePath, info) => {
    let path = filePath;
    if (!path.includes('/') && info.distFile) {
      path = `${info.distFile.slice(0, info.distFile.lastIndexOf('/') + 1)}${path}`;
    }
    return path.replace(/^localhost-\d+\//, '');
  },
  sourceFilter: (sourcePath) =>
    /(^|\/)src\//.test(sourcePath) &&
    !/(^|\/)src\/(lib\/mock|main)\.ts$/.test(sourcePath) &&
    !sourcePath.includes('type=style') &&
    !sourcePath.includes('node_modules'),
};
