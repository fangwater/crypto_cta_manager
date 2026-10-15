import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'

// The short-lived cookie arrives over SSH stdin and is never written to disk.
const auth=JSON.parse(fs.readFileSync(0,'utf8'))
const stage=process.argv[2]
const version=await(await fetch('http://127.0.0.1:19342/json/version')).json()
const ws=new WebSocket(version.webSocketDebuggerUrl)
await new Promise((resolve,reject)=>{ws.onopen=resolve;ws.onerror=reject})
let seq=0
const pending=new Map(),sessions=new Map(),results=[]
const call=(method,params={},sessionId)=>new Promise((resolve,reject)=>{
  const id=++seq;pending.set(id,{resolve,reject})
  ws.send(JSON.stringify({id,method,params,...(sessionId?{sessionId}:{})}))
})
ws.onmessage=({data})=>{
  const msg=JSON.parse(data)
  if(msg.id){const p=pending.get(msg.id);if(p){pending.delete(msg.id);msg.error?p.reject(new Error(JSON.stringify(msg.error))):p.resolve(msg.result)}}
  else if(msg.method==='Runtime.exceptionThrown')sessions.get(msg.sessionId)?.errors.push(msg.params.exceptionDetails)
}
const pause=ms=>new Promise(resolve=>setTimeout(resolve,ms))
async function scenario(width,height) {
  const context=await call('Target.createBrowserContext')
  try {
    const target=await call('Target.createTarget',{url:'about:blank',browserContextId:context.browserContextId})
    const {sessionId}=await call('Target.attachToTarget',{targetId:target.targetId,flatten:true})
    const state={errors:[]};sessions.set(sessionId,state)
    const c=(method,params)=>call(method,params,sessionId)
    const run=async expression=>{
      const r=await c('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true})
      if(r.exceptionDetails)throw new Error(JSON.stringify(r.exceptionDetails))
      return r.result.value
    }
    const waitFor=async expression=>{
      for(let n=0;n<180;n++) {
        if(await run(expression))return
        await pause(250)
      }
      throw new Error('live browser condition timed out')
    }
    await c('Runtime.enable');await c('Network.enable');await c('Page.enable')
    await c('Network.setCookie',{name:'cta_session',value:auth.token,url:'http://172.16.30.42:10041',httpOnly:true,sameSite:'Lax'})
    await c('Emulation.setTimezoneOverride',{timezoneId:'Asia/Shanghai'})
    await c('Emulation.setDeviceMetricsOverride',{width,height,deviceScaleFactor:1,mobile:width<600})
    await c('Page.addScriptToEvaluateOnNewDocument',{source:`
      window.__navRecords=[];window.__navStrokes={};
      const originalFetch=window.fetch;
      window.fetch=async(...args)=>{
        const started=performance.now();const response=await originalFetch(...args);
        if(/\\/(account-)?timeline\\?/.test(String(args[0]))) {
          response.clone().json().then(body=>{
            const t=body.theoretical,r=body.report;
            if(!r||!t)return;
            window.__navRecords.push({url:String(args[0]),status:response.status,elapsedMs:performance.now()-started,
              start:r.start_ts_us,end:r.end_ts_us,actualPoints:r.points.length,theoreticalPoints:t.points.length,
              loading:t.loading,reason:t.unavailable_reason,missing:t.missing_price_count,
              actualFirst:r.points[0],theoreticalFirst:t.points[0],theoreticalLast:t.points.at(-1),
              generationDurationMs:body.generation_duration_ms});
            if(t.points.length)window.__lastReady={url:String(args[0]),body};
          }).catch(()=>{});
        }
        return response;
      };
      const originalStroke=CanvasRenderingContext2D.prototype.stroke;
      CanvasRenderingContext2D.prototype.stroke=function(...args){window.__navStrokes[this.strokeStyle]=(window.__navStrokes[this.strokeStyle]||0)+1;return originalStroke.apply(this,args)};
    `})
    await c('Page.navigate',{url:'http://172.16.30.42:10041/manager/?source=binance_exec_trade01'})
    await waitFor(`window.__navRecords.some(r=>r.theoreticalPoints>0)&&!!document.querySelector('canvas')`)
    await run(`window.__navStrokes={};[...document.querySelectorAll('[aria-label="快捷时间范围"] button')].find(b=>b.textContent==='30D').click()`)
    await waitFor(`window.__navRecords.some(r=>r.theoreticalPoints>0&&r.end-r.start>29*86400000000&&r.start===new Date(document.querySelector('input[type="datetime-local"]').value).getTime()*1000)&&!document.querySelector('.chart-loading')`)
    await pause(500)
    const ready=await run(`(()=>({url:location.href,
      range:[...document.querySelectorAll('[aria-label="快捷时间范围"] .is-active')].map(e=>e.textContent),
      inputs:[...document.querySelectorAll('input[type="datetime-local"]')].map(e=>({value:e.value,ms:new Date(e.value).getTime()})),
      notices:[...document.querySelectorAll('.chart-notice')].map(e=>e.textContent),
      loading:!!document.querySelector('.chart-loading'),errors:[...document.querySelectorAll('.error-banner')].map(e=>e.textContent),
      overflow:document.documentElement.scrollWidth>innerWidth,strokes:window.__navStrokes,
      records:window.__navRecords,scriptAssets:[...document.scripts].map(e=>e.src).filter(Boolean),
      checkboxes:[...document.querySelectorAll('[aria-label="净值曲线选择"] label')].map(e=>({text:e.textContent,checked:e.querySelector('input').checked}))}))()`)
    const full=ready.records.filter(r=>r.theoreticalPoints>0).at(-1)
    assert.ok(full.end-full.start>29*86400000000)
    assert.equal(full.loading,false);assert.equal(full.reason,null);assert.equal(full.missing,0)
    assert.equal(full.actualFirst.ts_us,full.start);assert.equal(full.theoreticalFirst.ts_us,full.start)
    for(const first of [full.actualFirst,full.theoreticalFirst]) {
      assert.equal(first.nav_change_before_fee_quote,0);assert.equal(first.nav_change_after_fee_quote,0)
    }
    assert.equal(ready.inputs[0].ms*1000,full.start)
    assert.equal(ready.loading,false);assert.equal(ready.overflow,false);assert.equal(ready.errors.length,0)
    assert.ok(ready.notices.some(n=>n.includes('30D 已按当前可查询范围对齐')))
    assert.ok(ready.strokes['#7357a3']>0);assert.ok(ready.strokes['#a33f55']>0)
    assert.ok(ready.records.every(r=>new URLSearchParams(r.url.split('?')[1]).get('sourceIds')==='binance_exec_trade01'))
    const repeat=await run(`(async()=>{
      const previous=window.__lastReady;
      const before=await(await fetch('/manager/api/catalog/kline-status')).json();
      const began=performance.now();const next=await(await fetch(previous.url)).json();
      const elapsedMs=performance.now()-began;
      const after=await(await fetch('/manager/api/catalog/kline-status')).json();
      return {elapsedMs,requestDelta:after.requests-before.requests,
        factualIdentical:JSON.stringify(next.report)===JSON.stringify(previous.body.report),
        theoreticalIdentical:JSON.stringify(next.theoretical.points)===JSON.stringify(previous.body.theoretical.points),
        points:next.theoretical.points.length,missing:next.theoretical.missing_price_count,
        feeIdentity:next.theoretical.points.every(p=>Number.isFinite(p.nav_change_after_fee_quote)&&Math.abs(p.nav_change_before_fee_quote-p.nav_change_after_fee_quote-p.estimated_trading_fee_quote)<1e-7)};
    })()`)
    assert.equal(repeat.requestDelta,0);assert.ok(repeat.factualIdentical);assert.ok(repeat.theoreticalIdentical);assert.ok(repeat.feeIdentity)
    assert.equal(state.errors.length,0)
    await run(`document.querySelector('[aria-labelledby="chart-title"]').scrollIntoView({block:'center'})`)
    await pause(250)
    const screenshot=await c('Page.captureScreenshot',{format:'png',captureBeyondViewport:false})
    fs.writeFileSync(path.join(stage,`live_${width}.png`),Buffer.from(screenshot.data,'base64'))
    results.push({width,height,ready,repeat,jsErrors:state.errors,liveApi:true})
    sessions.delete(sessionId)
  } finally {await call('Target.disposeBrowserContext',context)}
}
try {
  await scenario(1440,1000);await scenario(390,1100)
  console.log(JSON.stringify({passed:true,results},null,2))
} finally {ws.close()}
