const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('/tmp/playwright-nav-check/node_modules/playwright');

async function main() {
  let input = '';
  for await (const chunk of process.stdin) input += chunk;
  const payload = JSON.parse(input);
  const browser = await chromium.launch({
    headless: true,
    executablePath: '/home/fanghaizhou/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome',
    env: { ...process.env, LD_LIBRARY_PATH: '/tmp/cta-pair-browser-deps/root/usr/lib/x86_64-linux-gnu:/tmp/cta-browser-libs/usr/lib/x86_64-linux-gnu' },
  });
  const checks = [];
  try {
    for (const [username, user] of Object.entries(payload.users)) {
      for (const [layout, viewport] of Object.entries({
        desktop: { width: 1440, height: 1000 },
        mobile: { width: 390, height: 1100 },
      })) {
        const context = await browser.newContext({ viewport });
        try {
          await context.addCookies([{
            name: 'cta_session', value: user.token,
            url: 'http://127.0.0.1:28151', httpOnly: true, sameSite: 'Lax',
          }]);
          const page = await context.newPage();
          const errors = [];
          page.on('pageerror', error => errors.push(error.message));
          await page.goto('http://127.0.0.1:28151/manager/config/position/', {
            waitUntil: 'networkidle', timeout: 60000,
          });
          const create = page.getByRole('button', { name: '新建', exact: true });
          await create.waitFor({ state: 'visible' });
          assert.equal(await create.isEnabled(), true);
          await create.click();
          await page.getByLabel('策略名', { exact: true }).fill('browser_draft_' + username);
          const save = page.getByRole('button', { name: '保存', exact: true });
          assert.equal(await save.isEnabled(), true);
          await page.getByRole('button', { name: new RegExp(user.own_name) }).click();
          assert.equal(await page.getByLabel('策略名', { exact: true }).inputValue(), user.own_name);
          assert.equal(await save.isEnabled(), true);
          const [response] = await Promise.all([
            page.waitForResponse(response => response.request().method() === 'POST' &&
              response.url().endsWith('/catalog/position-strategies')),
            save.click(),
          ]);
          assert.equal(response.status(), 200);
          await page.getByText('已保存；当前没有绑定账户，未写入 Redis', { exact: true }).waitFor();
          await page.reload({ waitUntil: 'networkidle' });
          await page.getByRole('button', { name: new RegExp(user.own_name) }).click();
          assert.equal(await save.isEnabled(), true);
          assert.deepEqual(errors, []);
          const overflow = await page.evaluate(() =>
            document.documentElement.scrollWidth > window.innerWidth);
          if (username === 'shaokai' && layout === 'desktop') {
            await page.screenshot({ path: path.join(__dirname, 'shaokai_desktop.png'), fullPage: true });
          }
          if (overflow) {
            await page.screenshot({ path: path.join(__dirname, username + '_' + layout + '_overflow.png'), fullPage: true });
            const elements = await page.evaluate(() => [...document.querySelectorAll('*')]
              .filter(element => element.getBoundingClientRect().right > window.innerWidth + 1)
              .slice(0, 8).map(element => ({ tag: element.tagName, className: element.className,
                right: element.getBoundingClientRect().right })));
            throw new Error(username + '/' + layout + ' overflow: ' + JSON.stringify(elements));
          }
          checks.push({ username, layout, new_button: true, create_save_enabled: true,
            owner_update_status: response.status(), owner_edit_after_reload: true,
            javascript_errors: 0, horizontal_overflow: false });
        } finally {
          await context.close();
        }
      }
    }
  } finally {
    await browser.close();
  }
  fs.writeFileSync(path.join(__dirname, 'browser_verification.json'),
    JSON.stringify({ ok: true, checks }, null, 2));
  process.stdout.write(JSON.stringify({ ok: true, checks }));
}

main().catch(error => {
  // Never include input/session data in failures.
  process.stderr.write(error.name + ': ' + error.message + '\n');
  process.exitCode = 1;
});
