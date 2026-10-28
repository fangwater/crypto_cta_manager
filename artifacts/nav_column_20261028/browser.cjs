const { chromium } = require('/home/fanghaizhou/.npm/_npx/e41f203b7505f1fb/node_modules/playwright');
const fs = require('node:fs');
const assert = require('node:assert/strict');
const base = 'http://13.115.227.29:4191';
const stage = '/home/fanghaizhou/crypto_cta_manager/.net-value-stage';
(async () => {
  const auth = JSON.parse(fs.readFileSync(stage+'/sessions.json','utf8'));
  const browser = await chromium.connectOverCDP('http://127.0.0.1:19337');
  try {
    for (const [user,width,height] of [['lgq',1440,1000],['lgq',390,1100],['prc',1440,1000]]) {
      const context = await browser.newContext({ viewport:{width,height}, deviceScaleFactor:1 });
      await context.addCookies([{ name:'cta_session', value:auth[user].token, url:base, httpOnly:true, sameSite:'Lax' }]);
      const page = await context.newPage();
      const errors=[]; const navRequests=[];
      page.on('pageerror', e=>errors.push(e.message));
      page.on('request',r=>{if(r.url().includes('/api/account-overview/nav?'))navRequests.push(r.url());});
      await page.goto(base+'/manager/workspace/',{waitUntil:'networkidle'});
      await page.locator('.overview-table tbody tr[data-source]').first().waitFor({timeout:30000});
      assert.equal(await page.locator('[data-nav-source]').count(),0,'overview contains no NAV cards');
      assert.match(await page.locator('.overview-stats').innerText(),/总权益/);
      assert.match(await page.locator('.overview-stats').innerText(),/今日总盈利/);
      assert.equal(navRequests.length,0,'overview must not request NAV curves');
      await page.screenshot({path:`${stage}/workspace-${user}-${width}.png`});
      const report=await context.request.get(base+'/manager/api/account-overview');
      assert.equal(report.status(),200);
      const ids=(await report.json()).accounts.map(a=>a.source_id);
      const started=Date.now();
      await page.locator('a[aria-label="净值"]:visible').click();
      await page.waitForURL('**/manager/net-value/');
      await page.waitForFunction(expected=>document.querySelectorAll('[data-nav-source]').length===expected,ids.length,{timeout:30000});
      await page.waitForFunction(()=>Array.from(document.querySelectorAll('[data-nav-value]')).every(n=>n.textContent!=='—'),null,{timeout:180000});
      const loadedMs=Date.now()-started;
      const charts=await page.locator('[data-nav-source]').evaluateAll(cards=>cards.map(card=>{
        const chart=card.querySelector('[data-nav-chart]');
        const bounds=card.getBoundingClientRect();
        return {source:card.getAttribute('data-nav-source'),chart:chart?.getAttribute('data-nav-chart'),instance:chart?.getAttribute('_echarts_instance_'),canvas:card.querySelectorAll('canvas').length,x:bounds.x,y:bounds.y};
      }));
      assert.equal(new Set(charts.map(c=>c.instance)).size,ids.length);
      for(const chart of charts){assert.ok(ids.includes(chart.source));assert.equal(chart.chart,chart.source);assert.equal(chart.canvas,1);assert.ok(chart.instance);}
      assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),'horizontal overflow');
      if(width<600)assert.ok(charts.every(c=>Math.abs(c.x-charts[0].x)<1));
      else if(charts.length>1){assert.equal(charts[0].y,charts[1].y);assert.notEqual(charts[0].x,charts[1].x);}
      assert.equal(await page.locator('.overview-table').count(),0,'NAV page has no daily overview table');
      assert.equal(await page.locator('a[aria-current="page"][aria-label="净值"]:visible').count(),1);
      const plot=page.locator('[data-nav-chart]').first();
      const plotBounds=await plot.boundingBox();
      await page.mouse.move(plotBounds.x+plotBounds.width*.6,plotBounds.y+plotBounds.height*.4);
      await page.locator('.overview-nav-tooltip-value').first().waitFor({state:'visible'});
      assert.match(await page.locator('.overview-nav-tooltip-value').first().innerText(),/累计变化/);
      await page.screenshot({path:`${stage}/nav-${user}-${width}.png`});
      await page.getByRole('searchbox',{name:'搜索账户'}).fill(ids[0]);
      await page.waitForFunction(id=>document.querySelectorAll('[data-nav-source]').length===1&&document.querySelector('[data-nav-source]').getAttribute('data-nav-source')===id,ids[0]);
      assert.deepEqual(errors,[]);
      console.log(JSON.stringify({user,viewport:width,workspaceRestored:true,overviewNavRequests:0,accounts:ids.length,independentCharts:charts.length,loadedMs,overflow:false,filterPassed:true}));
      await context.close();
    }
  } finally { await browser.close(); }
})().catch(e=>{console.error(e);process.exit(1);});
