import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'

const stage = process.argv[2]
const fixtures = Object.fromEntries(fs.readFileSync(path.join(stage, 'probe.jsonl'), 'utf8').trim().split('\n').map(JSON.parse).filter(r=>r.name).map(r=>[r.name,r.body]))
const actual = fixtures.user_30d_true
const availableUs = actual.theoretical.available_from_us
const expectedStartMs = availableUs / 1000 + 300_000
const mockedNow = actual.report.end_ts_us / 1000
const version = await (await fetch('http://127.0.0.1:19342/json/version')).json()
const ws = new WebSocket(version.webSocketDebuggerUrl)
await new Promise((resolve,reject)=>{ws.onopen=resolve;ws.onerror=reject})
const pending = new Map(), sessions = new Map(), results = []
let seq = 0
const call = (method, params={}, sessionId) => new Promise((resolve,reject)=>{
  const id=++seq;pending.set(id,{resolve,reject})
  ws.send(JSON.stringify({id,method,params,...(sessionId?{sessionId}:{})}))
})
ws.onmessage = async ({data}) => {
  const msg=JSON.parse(data)
  if(msg.id){const p=pending.get(msg.id);if(p){pending.delete(msg.id);msg.error?p.reject(new Error(JSON.stringify(msg.error))):p.resolve(msg.result)};return}
  const state=sessions.get(msg.sessionId)
  if(!state)return
  if(msg.method==='Runtime.exceptionThrown')state.errors.push(msg.params.exceptionDetails)
  if(msg.method!=='Fetch.requestPaused')return
  const {request,requestId}=msg.params
  const url=new URL(request.url)
  let body, type='application/json', raw=false
  try {
    if(url.pathname.includes('/api/')) {
      if(url.pathname.endsWith('/auth/status'))body={authenticated:true,setup_required:false,user:{user_id:0,username:'diagnostic_fixture',role:'admin',source_grants:[]}}
      else if(url.pathname.endsWith('/health'))body=fixtures.health_after
      else if(url.pathname.endsWith('/dashboard'))body=fixtures.dashboard
      else if(/\/(account-)?timeline$/.test(url.pathname)) {
        const start=Number(url.searchParams.get('startMs')),end=Number(url.searchParams.get('endMs'))
        const full=url.searchParams.get('includeTheoretical')==='true'
        const days=(end-start)/86400000
        const rejected=full && start<availableUs/1000
        const aligned=start===expectedStartMs
        const loading=full&&aligned&&++state.alignedFull===1
        const reason=!full?'理论曲线正在计算':rejected?actual.theoretical.unavailable_reason:loading?'分钟 K 线正在后台补齐，完成后自动更新曲线':null
        const boundary=days<2&&state.width<600?availableUs-10*60_000_000:availableUs
        body={...actual,execution_slippage:{...actual.execution_slippage,unavailable_reason:reason},report:{...actual.report,start_ts_us:start*1000,end_ts_us:end*1000},theoretical:{...actual.theoretical,
          available_from_us:full?(aligned&&!loading?availableUs+60_000_000:boundary):null,loading:!full||loading,unavailable_reason:reason,
          latest_point_ts_us:full&&!rejected&&!loading?end*1000:null,
          points:full&&!rejected&&!loading?[
            {ts_us:start*1000,nav_change_before_fee_quote:0,nav_change_after_fee_quote:0,estimated_trading_fee_quote:0},
            {ts_us:end*1000,nav_change_before_fee_quote:12,nav_change_after_fee_quote:10,estimated_trading_fee_quote:2},
          ]:[]}}
        state.requests.push({path:url.pathname,start,end,full,days,rejected,loading,source:url.searchParams.get('sourceIds')})
      } else body={}
    } else {
      const relative=url.pathname==='/manager/'?'index.html':url.pathname.replace(/^\/manager\//,'')
      body=fs.readFileSync(path.join(stage,'dist',relative));raw=true
      type=relative.endsWith('.js')?'text/javascript':relative.endsWith('.css')?'text/css':relative.endsWith('.svg')?'image/svg+xml':'text/html'
    }
    await call('Fetch.fulfillRequest',{requestId,responseCode:200,responseHeaders:[{name:'Content-Type',value:type}],body:(raw?body:Buffer.from(JSON.stringify(body))).toString('base64')},msg.sessionId)
  } catch(error){state.errors.push(String(error));await call('Fetch.failRequest',{requestId,errorReason:'Failed'},msg.sessionId)}
}
const pause=ms=>new Promise(resolve=>setTimeout(resolve,ms))
async function scenario(width,height) {
  const context=await call('Target.createBrowserContext')
  try {
    const target=await call('Target.createTarget',{url:'about:blank',browserContextId:context.browserContextId})
    const {sessionId}=await call('Target.attachToTarget',{targetId:target.targetId,flatten:true})
    const state={requests:[],errors:[],alignedFull:0,width};sessions.set(sessionId,state)
    const c=(method,params)=>call(method,params,sessionId)
    const run=async expression=>{const r=await c('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(r.exceptionDetails)throw new Error(JSON.stringify(r.exceptionDetails));return r.result.value}
    const waitFor=async expression=>{for(let n=0;n<100;n++){if(await run(expression))return;await pause(200)}throw new Error('browser condition timed out: '+expression)}
    await c('Runtime.enable');await c('Page.enable')
    await c('Emulation.setTimezoneOverride',{timezoneId:'Asia/Shanghai'})
    await c('Emulation.setDeviceMetricsOverride',{width,height,deviceScaleFactor:1,mobile:width<600})
    await c('Fetch.enable',{patterns:[{urlPattern:'*/manager/*',requestStage:'Request'}]})
    await c('Page.addScriptToEvaluateOnNewDocument',{source:`Date.now=()=>${mockedNow};window.__nav=[];const f=window.fetch;window.fetch=async(...args)=>{const r=await f(...args);if(/\\/(account-)?timeline\\?/.test(String(args[0]))){r.clone().json().then(b=>window.__nav.push({start:b.report.start_ts_us,loading:b.theoretical.loading,points:b.theoretical.points.length,reason:b.theoretical.unavailable_reason}))}return r}`})
    await c('Page.navigate',{url:'http://172.16.30.42:10041/manager/?source=binance_exec_trade01'})
    await waitFor(`window.__nav.some(r=>r.points>0)&&!!document.querySelector('canvas')`)
    await run(`[...document.querySelectorAll('[aria-label="快捷时间范围"] button')].find(b=>b.textContent==='30D').click()`)
    await waitFor(`window.__nav.some(r=>r.start===${expectedStartMs*1000}&&r.loading)`)
    const whilePending=await run(`({canvas:!!document.querySelector('canvas'),overlay:!!document.querySelector('.chart-loading'),notice:document.body.innerText.includes('30D 已按当前可查询范围对齐')})`)
    assert.equal(whilePending.canvas,true)
    await waitFor(`window.__nav.some(r=>r.start===${expectedStartMs*1000}&&r.points>0&&!r.loading)`)
    const ready=await run(`({loading:!!document.querySelector('.chart-loading'),notices:[...document.querySelectorAll('.chart-notice')].map(e=>e.textContent),start:document.querySelector('input[type="datetime-local"]').value,overflow:document.documentElement.scrollWidth>innerWidth,canvas:document.querySelectorAll('canvas').length})`)
    assert.equal(ready.loading,false);assert.equal(ready.overflow,false);assert.ok(ready.canvas>0)
    const aligned=state.requests.filter(r=>r.start===expectedStartMs)
    assert.ok(aligned.some(r=>!r.full));assert.ok(aligned.filter(r=>r.full).length>=2)
    assert.ok(state.requests.every(r=>r.source==='binance_exec_trade01'))
    // ALL must remain a factual all-history view, without retention adjustment.
    const beforeAll=state.requests.length
    await run(`[...document.querySelectorAll('[aria-label="快捷时间范围"] button')].find(b=>b.textContent==='ALL').click()`)
    await waitFor(`window.__nav.at(-1)?.loading===false`)
    await pause(300)
    const all=state.requests.slice(beforeAll)
    assert.ok(all.length>0);assert.ok(all.every(r=>r.start!==expectedStartMs))
    assert.equal(state.errors.length,0)
    results.push({width,height,whilePending,ready,requests:state.requests,errors:state.errors,simulatedApi:true})
    sessions.delete(sessionId)
  } finally {await call('Target.disposeBrowserContext',context)}
}
try {await scenario(1440,1000);await scenario(390,1100);console.log(JSON.stringify({passed:true,results},null,2))}
finally {ws.close()}
