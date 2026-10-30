import type { Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { button, expectEveryItemReachable, open, press, status } from './helpers';

// The library drawer's rows (summary first, grouped by day) and its resizable
// edge. Preloaded entries start 2026-10-26 09:00 local; the zone is UTC.

test.use({ timezoneId: 'UTC' });

const NOON = Date.UTC(2026, 9, 26, 12);
const DAY = 86_400_000;

function rows(page: Page) {
  return page.locator('#library li');
}

function days(page: Page) {
  return page.locator('#library .day');
}

function nav(page: Page) {
  return page.locator('#library');
}

function handle(page: Page) {
  return page.locator('.library-handle');
}

async function saved(page: Page): Promise<{ libraryWidth: number }[]> {
  return page.evaluate(() => [...(window.__mockBridge?.saved ?? [])] as { libraryWidth: number }[]);
}

async function width(page: Page): Promise<number> {
  return (await nav(page).boundingBox())?.width ?? 0;
}

async function centre(page: Page): Promise<{ x: number; y: number }> {
  const box = await handle(page).boundingBox();
  if (box === null) throw new Error('no handle');
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

async function dictate(page: Page): Promise<void> {
  await press(page, 'New', 'Record', 'Done');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue(/^This is part 1 of the dictation\./);
}

test.describe('rows', () => {
  test('lead with the summary, the time beneath, under a day label', async ({ page }) => {
    await page.clock.setFixedTime(NOON);
    await open(page, { library: '3' });
    const first = rows(page).first().getByRole('button');
    await expect(first.locator(':scope > :first-child')).toHaveClass('entry-summary');
    await expect(first.locator(':scope > :first-child')).toHaveText('Preloaded recording number 1.');
    await expect(first.locator(':scope > :last-child')).toHaveClass('entry-time');
    await expect(first.locator(':scope > :last-child')).toHaveText('9:00 AM');
    await expect(days(page)).toHaveText(['Today']);
  });

  test('are labelled Yesterday the next day and by date after that', async ({ page }) => {
    await page.clock.setFixedTime(NOON + DAY);
    await open(page, { library: '3' });
    await expect(days(page)).toHaveText(['Yesterday']);
    const later = await page.context().newPage();
    await later.clock.setFixedTime(NOON + 2 * DAY);
    await open(later, { library: '3' });
    await expect(days(later)).toHaveText(['Oct 26']);
    await dictate(later);
    await expect(days(later)).toHaveText(['Today', 'Oct 26']);
    await expect(rows(later).first().locator('.entry-time')).not.toHaveText('9:00 AM');
  });

  test('day labels are 13 px and every recording has a rule below it', async ({ page }) => {
    await page.clock.setFixedTime(NOON + 2 * DAY);
    await open(page, { library: '3' });
    await dictate(page);
    await expect(days(page)).toHaveText(['Today', 'Oct 26']);
    for (const d of await days(page).all()) {
      await expect(d).toHaveCSS('font-size', '13px');
      await expect(d).toHaveCSS('border-top-width', '0px');
      await expect(d).toHaveCSS('border-top-style', 'none');
      await expect(d).toHaveCSS('border-bottom-width', '0px');
      await expect(d).toHaveCSS('border-bottom-style', 'none');
    }
    await expect(days(page).first()).toHaveCSS('margin-top', '2px');
    const second = days(page).nth(1);
    await expect(second).toHaveCSS('margin-top', '8px');
    await expect(second).toHaveCSS('padding-top', '0px');
    await expect(second).toHaveCSS('border-top-width', '0px');
    await expect(rows(page)).toHaveCount(4);
    for (const r of await rows(page).all()) {
      await expect(r).toHaveCSS('border-bottom-width', '1px');
      await expect(r).toHaveCSS('border-bottom-style', 'solid');
      await expect(r).toHaveCSS('border-top-width', '0px');
    }
    await page.emulateMedia({ colorScheme: 'light' });
    await expect(rows(page).last()).toHaveCSS('border-bottom-color', 'rgb(222, 218, 213)');
    await page.emulateMedia({ colorScheme: 'dark' });
    await expect(rows(page).last()).toHaveCSS('border-bottom-color', 'rgb(58, 54, 51)');
    const r = await page.locator('#library ul').nth(0).locator('li').last().boundingBox();
    const l = await second.boundingBox();
    if (!r || !l) throw new Error('no box');
    expect(l.y - (r.y + r.height)).toBeCloseTo(8, 0);
  });

  test('relabel when the clock passes midnight', async ({ page }) => {
    await page.clock.install({ time: Date.UTC(2026, 9, 26, 23, 59, 30) });
    await open(page, { library: '3' });
    await expect(days(page)).toHaveText(['Today']);
    await page.clock.runFor(60_000);
    await expect(days(page)).toHaveText(['Yesterday']);
  });

  test('a recording after midnight, before the tick, never sits above a stale Today', async ({ page }) => {
    await page.clock.install({ time: Date.UTC(2026, 9, 26, 23, 59, 50) });
    await open(page, { library: '3' });
    await page.clock.runFor(15_000);
    await dictate(page);
    await expect(days(page)).toHaveText(['Today', 'Yesterday']);
  });

  test('show two lines of a long summary', async ({ page }) => {
    await page.clock.setFixedTime(NOON);
    await open(page, { library: '3', libraryWidth: '110' });
    const summary = rows(page).first().locator('.entry-summary');
    const [scroll, client] = await summary.evaluate((el) => [el.scrollHeight, el.clientHeight]);
    expect(scroll).toBeGreaterThan(client);
    expect(client).toBeLessThanOrEqual(37);
    await expect(summary).toHaveCSS('-webkit-line-clamp', '2');
    await expect(summary).toHaveCSS('line-height', '18px');
  });
});

test.describe('resize', () => {
  test.beforeEach(async ({ page }) => {
    await page.setViewportSize({ width: 960, height: 600 });
  });

  test('the handle sits on the edge with the column-resize cursor', async ({ page }) => {
    await open(page, { library: '3' });
    await expect(handle(page)).toHaveCSS('cursor', 'col-resize');
    const h = await handle(page).boundingBox();
    const n = await nav(page).boundingBox();
    expect(h && n && h.x < n.x + n.width && h.x + h.width > n.x + n.width).toBe(true);
  });

  test.describe('light', () => {
    test.use({ colorScheme: 'light' });
    test('hover and focus show the accent', async ({ page }) => {
      await open(page, { library: '3' });
      await handle(page).hover();
      await expect(handle(page)).toHaveCSS('background-color', 'rgb(180, 68, 27)');
      await page.mouse.move(500, 300);
      await handle(page).focus();
      await page.keyboard.press('Tab');
      await handle(page).focus();
      await expect(handle(page)).toHaveCSS('background-color', 'rgb(180, 68, 27)');
    });

    test('a mouse drag leaves no focus and shows the accent only while dragging', async ({ page }) => {
      await open(page, { library: '3' });
      const { x, y } = await centre(page);
      await page.mouse.move(x, y);
      await page.mouse.down();
      await page.mouse.move(900, y);
      await expect(handle(page)).toHaveClass(/\bdragging\b/);
      await expect(handle(page)).toHaveCSS('background-color', 'rgb(180, 68, 27)');
      await page.mouse.up();
      await page.mouse.move(500, 300);
      await expect(handle(page)).not.toHaveClass(/\bdragging\b/);
      expect(await page.evaluate(() => document.activeElement?.classList.contains('library-handle'))).toBe(false);
      await expect(handle(page)).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
      await expect(handle(page)).toHaveCSS('outline-style', 'none');
    });
  });

  test('dragging resizes live and saves once on release', async ({ page }) => {
    await open(page, { library: '3' });
    const { x, y } = await centre(page);
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + 100, y);
    expect(await width(page)).toBe(320);
    await expect(handle(page)).toHaveAttribute('aria-valuenow', '320');
    expect(await saved(page)).toEqual([]);
    await page.mouse.up();
    await expect.poll(() => saved(page).then((s) => s.map((e) => e.libraryWidth))).toEqual([320]);
  });

  test('is held to 110 and 440 while dragging', async ({ page }) => {
    await open(page, { library: '3' });
    const { x, y } = await centre(page);
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(0, y);
    expect(await width(page)).toBe(110);
    await page.mouse.move(900, y);
    expect(await width(page)).toBe(440);
    await page.mouse.up();
  });

  test('saves a whole number', async ({ page }) => {
    await open(page, { library: '3' });
    const { y } = await centre(page);
    await page.mouse.move(220, y);
    await page.mouse.down();
    await page.mouse.move(320.5, y);
    await page.mouse.up();
    await expect.poll(async () => (await saved(page)).length).toBe(1);
    expect(Number.isInteger((await saved(page)).at(-1)?.libraryWidth)).toBe(true);
  });

  test('a click, a right-button press and a bare hover change nothing', async ({ page }) => {
    await open(page, { library: '3' });
    const { x, y } = await centre(page);
    await page.mouse.click(x, y);
    await page.mouse.move(x, y);
    await page.mouse.down({ button: 'right' });
    await page.mouse.move(x + 80, y);
    await page.mouse.up({ button: 'right' });
    await page.mouse.move(x + 40, y);
    expect(await width(page)).toBe(220);
    expect(await saved(page)).toEqual([]);
  });

  test('the arrow keys, Home and End', async ({ page }) => {
    await open(page, { library: '3' });
    await handle(page).focus();
    const steps: [string, number][] = [['ArrowRight', 230], ['ArrowLeft', 220], ['ArrowLeft', 210], ['Home', 110], ['ArrowLeft', 110], ['End', 440]];
    for (const [key, want] of steps) {
      await page.keyboard.press(key);
      expect(await width(page), key).toBe(want);
      await expect(handle(page)).toHaveAttribute('aria-valuenow', String(want));
    }
    expect((await saved(page)).map((s) => s.libraryWidth)).toEqual([230, 220, 210, 110, 440]);
  });

  test('a held key makes one save', async ({ page }) => {
    await open(page, { library: '3' });
    await handle(page).focus();
    for (let i = 0; i < 3; i++) await page.keyboard.down('ArrowRight');
    await page.keyboard.up('ArrowRight');
    expect(await width(page)).toBe(250);
    expect((await saved(page)).map((s) => s.libraryWidth)).toEqual([250]);
  });

  test('a held key saves when the handle loses focus', async ({ page }) => {
    await open(page, { library: '3' });
    await handle(page).focus();
    await page.keyboard.down('ArrowRight');
    await handle(page).blur();
    expect((await saved(page)).map((s) => s.libraryWidth)).toEqual([230]);
  });

  test('Space on the handle still records', async ({ page }) => {
    await open(page, { library: '3' });
    await press(page, 'New');
    await handle(page).focus();
    await page.keyboard.press('Space');
    await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
    expect(await saved(page)).toEqual([]);
  });

  async function drag(page: Page): Promise<void> {
    const { x, y } = await centre(page);
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + 60, y);
    await page.mouse.up();
  }

  const resizing = (page: Page) => page.evaluate(() => document.documentElement.classList.contains('library-resizing'));
  const userSelect = (page: Page, sel: string) => page.evaluate((q) => getComputedStyle(document.querySelector(q) as Element).userSelect, sel);

  test('a drag selects no text anywhere', async ({ page }) => {
    await open(page, { library: '3' });
    await page.locator('#library li').nth(1).getByRole('button').click();
    await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue('This is preloaded recording number 2.');
    const { x, y } = await centre(page);
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + 200, y + 120);
    expect(await width(page)).toBe(420);
    await page.mouse.move(x - 150, y + 160);
    expect(await width(page)).toBe(110);
    for (const q of ['body', '#library li button', '#entry-transcript']) expect(await userSelect(page, q), q).toBe('none');
    expect(await resizing(page)).toBe(true);
    await page.mouse.up();
    expect(await page.evaluate(() => window.getSelection()?.toString() ?? '')).toBe('');
    expect(await page.evaluate(() => {
      const el = document.querySelector('#entry-transcript') as HTMLTextAreaElement;
      return el.selectionStart === el.selectionEnd;
    })).toBe(true);
    expect(await resizing(page)).toBe(false);
    await page.mouse.move(x, y);
    await page.mouse.down({ button: 'right' });
    expect(await resizing(page)).toBe(false);
    await expect(handle(page)).not.toHaveClass(/\bdragging\b/);
    await page.mouse.up({ button: 'right' });
  });

  test('selection and typing work after a drag', async ({ page }) => {
    await open(page, { library: '3' });
    const before = await userSelect(page, 'body');
    await drag(page);
    expect(await userSelect(page, 'body')).toBe(before);
    await dictate(page);
    await page.locator('#transcript').click();
    await page.keyboard.press('ControlOrMeta+A');
    const sel = await page.locator('#transcript').evaluate((el: HTMLTextAreaElement) => [el.selectionEnd - el.selectionStart, el.value.length]);
    expect(sel[0]).toBe(sel[1]);
    expect(sel[1]).toBeGreaterThan(0);
    await page.locator('#transcript').click();
    await page.keyboard.press('End');
    await page.keyboard.type('!');
    await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue(/!$/);
  });

  test('losing capture ends the drag', async ({ page }) => {
    await open(page, { library: '3' });
    await page.evaluate(() => {
      window.addEventListener(
        'pointerdown',
        (e) => {
          (window as unknown as { __pid: number }).__pid = e.pointerId;
        },
        { capture: true },
      );
    });
    const { x, y } = await centre(page);
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + 40, y);
    expect(await resizing(page)).toBe(true);
    await page.evaluate(() => {
      const pid = (window as unknown as { __pid: number }).__pid;
      (document.querySelector('.library-handle') as HTMLElement).releasePointerCapture(pid);
    });
    await page.mouse.move(x + 60, y);
    await expect.poll(() => resizing(page)).toBe(false);
    await expect(handle(page)).not.toHaveClass(/\bdragging\b/);
    const w = await width(page);
    await page.mouse.move(x + 100, y);
    expect(await width(page)).toBe(w);
    await page.mouse.up();
  });

  test('a keyboard-focused handle keeps focus through a drag', async ({ page }) => {
    await open(page, { library: '3' });
    await handle(page).focus();
    await drag(page);
    expect(await page.evaluate(() => document.activeElement?.classList.contains('library-handle'))).toBe(true);
  });

  test('the keys still resize after a mouse drag', async ({ page }) => {
    await open(page, { library: '3' });
    await drag(page);
    await expect.poll(() => saved(page).then((s) => s.length)).toBe(1);
    await handle(page).focus();
    await page.keyboard.press('ArrowRight');
    expect(await width(page)).toBe(290);
    await expect.poll(() => saved(page).then((s) => s.map((e) => e.libraryWidth))).toEqual([280, 290]);
  });

  test('starts at the saved width and keeps it through hide and show', async ({ page }) => {
    await open(page, { library: '3', libraryWidth: '300' });
    expect(await width(page)).toBe(300);
    await button(page, 'Library').click();
    await button(page, 'Library').click();
    expect(await width(page)).toBe(300);
  });

  test('a save that fails says why and keeps the width', async ({ page }) => {
    await open(page, { library: '3', fail: 'settings_set' });
    const { x, y } = await centre(page);
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + 50, y);
    await page.mouse.up();
    await expect(page.getByRole('banner').getByRole('alert')).toHaveText('Mock failure: settings_set');
    expect(await width(page)).toBe(270);
  });
});

test.describe('a small window', () => {
  const PARAMS = { libraryWidth: '440', library: '30', long: '1' };

  test.beforeEach(async ({ page }) => {
    await page.setViewportSize({ width: 520, height: 380 });
  });

  test('keeps 240 px for the recorder', async ({ page }) => {
    await open(page, PARAMS);
    expect(await width(page)).toBe(280);
    expect((await page.locator('main').boundingBox())?.width).toBe(240);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await expectEveryItemReachable(rows(page));
  });

  test('a click on the capped handle saves nothing; a drag of -40 saves 240', async ({ page }) => {
    await open(page, PARAMS);
    const { x, y } = await centre(page);
    await page.mouse.click(x, y);
    expect(await saved(page)).toEqual([]);
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x - 40, y);
    await page.mouse.up();
    await expect.poll(() => saved(page).then((s) => s.map((e) => e.libraryWidth))).toEqual([240]);
  });

  test('an arrow key rebases on the rendered width', async ({ page }) => {
    await open(page, PARAMS);
    await handle(page).focus();
    await page.keyboard.press('ArrowRight');
    expect((await saved(page)).map((s) => s.libraryWidth)).toEqual([290]);
  });

  test('the recorder stays in view in every state', async ({ page }) => {
    await open(page, PARAMS);
    const controls = async (): Promise<void> => {
      for (const b of await page.locator('.controls button').all()) {
        if (await b.isVisible()) await expect(b).toBeInViewport({ ratio: 1 });
      }
    };
    await press(page, 'New', 'Record');
    await controls();
    await press(page, 'Pause');
    await controls();
    await press(page, 'Resume');
    await controls();
    await press(page, 'Done');
    await expect(page.getByRole('textbox', { name: 'Transcript' })).toBeInViewport();
    await expect(page.getByRole('button', { name: 'Copy' })).toBeInViewport();
    await controls();
  });
});
