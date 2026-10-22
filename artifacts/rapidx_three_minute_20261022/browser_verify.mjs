import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'

// The short-lived cookie arrives over SSH stdin and is never written to disk.
const auth=JSON.parse(fs.readFileSync(0,'utf8'))
const stage=process.argv[2]
const version=await(await fetch('http://127.0.0.1:19343/json/version')).json()
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
const base = 'http://13.115.227.29:4191'
async function scenario(width, height, source, cost=false) {
  const context = await call('Target.createBrowserContext')
  try {
    const target = await call('Target.createTarget', {url:'about:blank',browserContextId:context.browserContextId})
    const {sessionId}=await call('Target.attachToTarget',{targetId:target.targetId,flatten:true})
    const state={errors:[]};sessions.set(sessionId,state)
    const c=(method,params)=>call(method,params,sessionId)
    const run=async expression=>{
      const r=await c('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true})
      if(r.exceptionDetails)throw new Error(JSON.stringify(r.exceptionDetails))
      return r.result.value
    }
    const waitFor=async expression=>{
      for(let n=0;n<2400;n++) {if(await run(expression))return;await pause(250)}
      throw new Error('browser condition timed out: '+expression)
    }
    await c('Runtime.enable');await c('Network.enable');await c('Page.enable')
    await c('Network.setCookie',{name:'cta_session',value:auth.token,url:base,httpOnly:true,sameSite:'Lax'})
    await c('Emulation.setTimezoneOverride',{timezoneId:'Asia/Shanghai'})
    await c('Emulation.setDeviceMetricsOverride',{width,height,deviceScaleFactor:1,mobile:width<600})
    await c('Page.addScriptToEvaluateOnNewDocument',{source:`
      window.__records=[];window.__strokes={};
      const originalFetch=window.fetch;
      window.fetch=async(...args)=>{
        const response=await originalFetch(...args);
        if(/timeline\\?|acquisition-cost\\?/.test(String(args[0]))) response.clone().json().then(body=>window.__records.push({url:String(args[0]),status:response.status,body})).catch(()=>{});
        return response;
      };
      const originalStroke=CanvasRenderingContext2D.prototype.stroke;
      CanvasRenderingContext2D.prototype.stroke=function(...args){window.__strokes[this.strokeStyle]=(window.__strokes[this.strokeStyle]||0)+1;return originalStroke.apply(this,args)};
    `})
    await c('Page.navigate',{url:base+'/manager/'+(cost?'acquisition-cost':'')+'?source='+source})
    if(cost){
      await waitFor(`[...document.querySelectorAll('button')].some(b=>b.textContent.includes('查询生成')&&!b.disabled)`)
      await run(`[...document.querySelectorAll('button')].find(b=>b.textContent.includes('查询生成')).click()`)
      await waitFor(`window.__records.some(r=>r.body.reports?.length===2)&&!!document.querySelector('canvas')`)
      await waitFor(`document.body.textContent.includes('两个三分钟参考')`)
      const before=await run(`window.__records.findLast(r=>r.body.reports)?.body.reports.map(r=>({model:r.execution_model,rows:r.rows.length,missing:r.totals.missing_virtual_delta_count}))`)
      assert.deepEqual(before.map(r=>r.model),['vwap3m','twap3m'])
      await run(`[...document.querySelectorAll('button')].find(b=>b.textContent==='理论3min_twap').click()`)
      await pause(300)
      assert.ok(await run(`document.body.textContent.includes('理论3min_twap 将连续三个完整分钟')`))
      assert.ok(await run(`window.__strokes['#d97706']>0`))
    } else {
      const count=source==='binance_exec_trade03'?1:2
      await waitFor(`window.__records.some(r=>r.body.theoretical?.length===${count}&&r.body.theoretical.every(t=>!t.loading&&(t.points.length>0||t.unavailable_reason)))&&!!document.querySelector('canvas')`)
      await pause(500)
      const visible=await run(`[...document.querySelectorAll('[aria-label="净值曲线选择"] label')].map(e=>({text:e.textContent,checked:e.querySelector('input').checked}))`)
      if(count===2){
        for(const label of ['理论3min_vwap','理论3min_twap'])assert.equal(visible.filter(v=>v.text.includes(label)&&v.checked).length,2)
        if(source==='binance_exec_trade07') assert.ok(await run(`window.__strokes['#7357a3']>0&&window.__strokes['#a33f55']>0&&window.__strokes['#0891b2']>0&&window.__strokes['#d97706']>0`))
        else assert.ok(await run(`document.body.textContent.includes('不可用')`))
      } else assert.ok(visible.every(v=>!v.text.includes('3min')))
    }
    const result=await run(`({overflow:document.documentElement.scrollWidth>innerWidth,labels:[...document.querySelectorAll('[aria-label="净值曲线选择"] label')].map(e=>e.textContent),canvasCount:document.querySelectorAll('canvas').length,records:window.__records.map(r=>({url:r.url,status:r.status,references:r.body.theoretical?.map(t=>({model:t.execution_model,points:t.points.length,loading:t.loading,missing:t.missing_price_count,reason:t.unavailable_reason})),reports:r.body.reports?.map(t=>({model:t.execution_model,rows:t.rows.length}))})),assets:[...document.scripts].map(e=>e.src).filter(Boolean)})`)
    assert.equal(result.overflow,false);assert.equal(state.errors.length,0)
    await run(`document.querySelector('${cost ? '#delta-analysis' : '.chart-body.has-picker'}')?.scrollIntoView({block:'start'})`)
    await pause(300)
    const shot=await c('Page.captureScreenshot',{format:'png',captureBeyondViewport:false})
    fs.writeFileSync(path.join(stage,`${source}-${cost?'cost':'nav'}-${width}.png`),Buffer.from(shot.data,'base64'))
    results.push({width,height,source,cost,...result,errors:state.errors})
  } finally {await call('Target.disposeBrowserContext',{browserContextId:context.browserContextId})}
}
try {
  for(const [width,height] of [[1440,1000],[390,1100]]){
    await scenario(width,height,'binance_exec_trade07')
    await scenario(width,height,'binance_exec_trade06')
    await scenario(width,height,'binance_exec_trade06',true)
  }
  await scenario(1440,1000,'binance_exec_trade03')
  console.log(JSON.stringify({passed:true,results}))
} finally {ws.close()}
