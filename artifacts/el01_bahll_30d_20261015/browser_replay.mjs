import fs from 'node:fs'

const records = fs.readFileSync(process.argv[2], 'utf8').trim().split('\n').map(JSON.parse)
const fixtures = Object.fromEntries(records.filter(r => r.name).map(r => [r.name, r.body]))
const browser = await (await fetch('http://127.0.0.1:19342/json/version')).json()
const ws = new WebSocket(browser.webSocketDebuggerUrl)
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject })
let seq = 0
const pending = new Map(), errors = [], requests = [], strokes = {}
const call = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
  const id = ++seq
  pending.set(id, { resolve, reject })
  ws.send(JSON.stringify({ id, method, params, ...(sessionId ? {sessionId} : {}) }))
})
ws.onmessage = async ({ data }) => {
  const msg = JSON.parse(data)
  if (msg.id) {
    const p = pending.get(msg.id)
    if (p) { pending.delete(msg.id); msg.error ? p.reject(new Error(JSON.stringify(msg.error))) : p.resolve(msg.result) }
  } else if (msg.method === 'Runtime.exceptionThrown') {
    errors.push(msg.params.exceptionDetails)
  } else if (msg.method === 'Fetch.requestPaused') {
    const { request, requestId } = msg.params
    const url = new URL(request.url)
    let body
    if (url.pathname.endsWith('/auth/status')) body = {authenticated:true,setup_required:false,user:{user_id:0,username:'diagnostic_replay',role:'admin',source_grants:[]}}
    else if (url.pathname.endsWith('/dashboard')) body = fixtures.dashboard
    else if (url.pathname.endsWith('/health')) body = fixtures.health_after
    else if (url.pathname.endsWith('/timeline') || url.pathname.endsWith('/account-timeline')) {
      body = fixtures[url.searchParams.get('includeTheoretical') === 'true' ? 'user_30d_true' : 'user_30d_false']
      requests.push({ path:url.pathname, includeTheoretical:url.searchParams.get('includeTheoretical'), at:Date.now() })
    } else body = {}
    await call('Fetch.fulfillRequest', {requestId,responseCode:200,responseHeaders:[{name:'Content-Type',value:'application/json'}],body:Buffer.from(JSON.stringify(body)).toString('base64')}, msg.sessionId)
  }
}
let context
try {
  context = await call('Target.createBrowserContext')
  const target = await call('Target.createTarget', {url:'about:blank',browserContextId:context.browserContextId})
  const {sessionId} = await call('Target.attachToTarget', {targetId:target.targetId,flatten:true})
  const c = (method, params) => call(method, params, sessionId)
  const run = async expression => {
    const r = await c('Runtime.evaluate', {expression,returnByValue:true,awaitPromise:true})
    if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails))
    return r.result.value
  }
  await c('Runtime.enable')
  await c('Page.enable')
  await c('Fetch.enable', {patterns:[{urlPattern:'*/manager/api/*',requestStage:'Request'}]})
  await c('Emulation.setDeviceMetricsOverride', {width:1440,height:1000,deviceScaleFactor:1,mobile:false})
  await c('Page.addScriptToEvaluateOnNewDocument', {source:`
    window.__strokes={};window.__longTasks=[];
    new PerformanceObserver(list=>window.__longTasks.push(...list.getEntries().map(e=>({start:e.startTime,duration:e.duration})))).observe({type:'longtask',buffered:true});
    const originalStroke=CanvasRenderingContext2D.prototype.stroke;
    CanvasRenderingContext2D.prototype.stroke=function(...args){window.__strokes[this.strokeStyle]=(window.__strokes[this.strokeStyle]||0)+1;return originalStroke.apply(this,args)};
  `})
  await c('Page.navigate', {url:'http://172.16.30.42:10041/manager/?source=binance_exec_trade01'})
  const pause = ms => new Promise(resolve=>setTimeout(resolve,ms))
  for(let n=0;n<40;n++) {
    await pause(250)
    if(await run(`!!document.querySelector('[aria-label="快捷时间范围"] button') && !document.querySelector('.chart-loading') && document.querySelectorAll('canvas').length>0`)) break
  }
  const clicked = await run(`(()=>{const b=[...document.querySelectorAll('[aria-label="快捷时间范围"] button')].find(b=>b.textContent==='30D');b?.click();return !!b})()`)
  await pause(2000)
  const result = await run(`(()=>({title:document.title,selectedRange:[...document.querySelectorAll('[aria-label="快捷时间范围"] .is-active')].map(e=>e.textContent),notices:[...document.querySelectorAll('.chart-notice')].map(e=>e.textContent),loading:!!document.querySelector('.chart-loading'),canvasCount:document.querySelectorAll('canvas').length,chartText:document.querySelector('.chart-foot')?.textContent,strokes:window.__strokes,longTasks:window.__longTasks,jsHeapUsed:performance.memory?.usedJSHeapSize,overflow:document.documentElement.scrollWidth>innerWidth}))()`)
  console.log(JSON.stringify({fixtureReplay:true,onlineBundle:true,clicked,...result,errors,requests},null,2))
} finally {
  if(context) await call('Target.disposeBrowserContext', context)
  ws.close()
}
