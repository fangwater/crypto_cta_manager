import fs from 'node:fs'
const root = '/home/fanghaizhou/snap/chromium/common/ctk-deployment-20261013T060934Z'
const auth = JSON.parse(fs.readFileSync(`${root}/session.json`, 'utf8'))
const target = await (await fetch('http://127.0.0.1:19334/json/new?about:blank', {method:'PUT'})).json()
const ws = new WebSocket(target.webSocketDebuggerUrl)
await new Promise((resolve, reject) => { ws.onopen=resolve; ws.onerror=reject })
let seq=0;const pending=new Map();const errors=[];const requests=[]
ws.onmessage=({data})=>{const msg=JSON.parse(data);if(msg.id){const p=pending.get(msg.id);if(p){pending.delete(msg.id);msg.error?p.reject(new Error(JSON.stringify(msg.error))):p.resolve(msg.result)}}else if(msg.method==='Runtime.exceptionThrown')errors.push(msg.params.exceptionDetails.text);else if(msg.method==='Network.requestWillBeSent'&&msg.params.request.url.includes('/acquisition-cost?'))requests.push(msg.params.request.url)}
const call=(method,params={})=>new Promise((resolve,reject)=>{const id=++seq;pending.set(id,{resolve,reject});ws.send(JSON.stringify({id,method,params}))})
const run=async expression=>{const r=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(r.exceptionDetails)throw new Error(r.exceptionDetails.text);return r.result.value}
const pause=ms=>new Promise(resolve=>setTimeout(resolve,ms))
const waitFor=async(expression,seconds=180)=>{for(let n=0;n<seconds*2;n++){if(await run(expression))return;await pause(500)}throw new Error(`Timed out: ${expression}`)}
const labelSelect = text => `Array.from(document.querySelectorAll('label')).find(l=>l.textContent.startsWith(${JSON.stringify(text)}))?.querySelector('select')`
const select=async(text,value)=>{const selector=labelSelect(text);await run(`(()=>{const s=${selector};if(!s)throw Error('missing select');s.value=${JSON.stringify(value)};s.dispatchEvent(new Event('change',{bubbles:true}));})()`)}
try {
 await call('Network.enable');await call('Runtime.enable');await call('Page.enable')
 await call('Network.setCookie',{name:'cta_session',value:auth.token,url:'http://13.115.227.29:4191',httpOnly:true,sameSite:'Lax'})
 await call('Emulation.setDeviceMetricsOverride',{width:1440,height:1000,deviceScaleFactor:1,mobile:false})
 await call('Page.navigate',{url:'http://13.115.227.29:4191/manager/acquisition-cost/?source=binance_exec_trade03'})
 await waitFor(`Array.from(document.querySelectorAll('button')).some(b=>b.textContent.trim()==='查询生成'&&!b.disabled)`)
 for(let attempt=0;attempt<90;attempt++) {
  const ready=await run(`fetch('/manager/api/catalog/kline-status').then(r=>r.json()).then(s=>s.target_history.ready&&!s.target_history.loading)`)
  if(ready)break
  if(attempt===89)throw Error('Target index not ready for browser check')
  await pause(10000)
 }
 await run(`Array.from(document.querySelectorAll('button')).find(b=>b.textContent.trim()==='查询生成').click()`)
 await waitFor(`!!document.getElementById('delta-details')&&!!document.getElementById('delta-analysis')`)
 await select('复盘币种','CTKUSDT')
 await waitFor(`!${labelSelect('复盘策略')}?.disabled&&document.querySelector('#delta-details [role=status]')?.textContent.startsWith('CTKUSDT · 全部策略')`)
 await select('复盘策略','rbf_small')
 await waitFor(`!${labelSelect('复盘策略')}?.disabled&&document.getElementById('delta-details')?.textContent.includes('CTKUSDT · rbf_small')&&!!document.getElementById('delta-analysis')`)
 await select('目标排序','price_shortfall')
 await waitFor(`!${labelSelect('目标排序')}?.disabled&&!!document.getElementById('delta-analysis')`)
 const selected=await run(`Array.from((${labelSelect('当前页目标')}).options).map(o=>o.textContent)`)
 if(!selected.length||selected.some(s=>!s.includes('CTKUSDT')||!s.includes('rbf_small')))throw Error('Wrong target selection')
 const smallRows=selected.length
 const smallState=await run(`document.getElementById('delta-details').innerText`)
 const next=await run(`Array.from(document.querySelectorAll('#delta-details button')).find(b=>b.textContent==='下一页')?.disabled===false`)
 if(next){await run(`Array.from(document.querySelectorAll('#delta-details button')).find(b=>b.textContent==='下一页').click()`);await waitFor(`!${labelSelect('复盘策略')}?.disabled&&!!document.getElementById('delta-analysis')`);const values=await run(`Array.from((${labelSelect('当前页目标')}).options).map(o=>o.textContent)`);if(values.some(v=>!v.includes('rbf_small')||!v.includes('CTKUSDT')))throw Error('Paging lost filters')}
 await select('复盘策略','rbf_mid_1min')
 await waitFor(`!${labelSelect('复盘策略')}?.disabled&&document.querySelector('#delta-details [role=status]')?.textContent.startsWith('CTKUSDT · rbf_mid_1min')`)
 const midCtkEmpty=await run(`!document.getElementById('delta-analysis')&&document.body.innerText.includes('当前币种、策略和范围没有可展示的 delta')`)
 await select('复盘币种','')
 await waitFor(`!${labelSelect('复盘策略')}?.disabled&&document.querySelector('#delta-details [role=status]')?.textContent.startsWith('全部币种 · rbf_mid_1min')&&!!document.getElementById('delta-analysis')`)
 const mid=await run(`Array.from((${labelSelect('当前页目标')}).options).map(o=>o.textContent)`)
 if(!mid.length||mid.some(s=>!s.includes('rbf_mid_1min')))throw Error('Switching strategy failed')
 await select('复盘策略','rbf_small')
 await waitFor(`!${labelSelect('复盘策略')}?.disabled&&document.querySelector('#delta-details [role=status]')?.textContent.startsWith('全部币种 · rbf_small')&&!!document.getElementById('delta-analysis')`)
 await select('复盘币种','CTKUSDT')
 await waitFor(`!${labelSelect('复盘策略')}?.disabled&&document.querySelector('#delta-details [role=status]')?.textContent.startsWith('CTKUSDT · rbf_small')&&!!document.getElementById('delta-analysis')`)
 await run(`document.getElementById('delta-details').scrollIntoView()`)
 await pause(500)
 const desktop=await call('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});fs.writeFileSync(`${root}/desktop.png`,Buffer.from(desktop.data,'base64'))
 await call('Emulation.setDeviceMetricsOverride',{width:390,height:1100,deviceScaleFactor:1,mobile:true})
 await pause(1000)
 const mobile=await call('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});fs.writeFileSync(`${root}/mobile.png`,Buffer.from(mobile.data,'base64'))
 const canvasCount=await run(`document.querySelectorAll('#delta-analysis canvas').length`)
 const overflow=await run(`document.documentElement.scrollWidth>window.innerWidth`)
 if(overflow)throw Error('Mobile horizontal overflow')
 if(!canvasCount)throw Error('Missing rendered chart canvas')
 if(errors.length)throw Error('Browser JavaScript exception')
 const summary={browser:'Chromium',small_ctk_options:smallRows,mid_options:mid.length,mid_ctk_empty_clears_chart:midCtkEmpty,small_scope:smallState,pagination_checked:next,chart_canvas_count:canvasCount,mobile_horizontal_overflow:overflow,js_exceptions:errors.length,scoped_acquisition_requests:requests.length,all_requests_source_scoped:requests.every(u=>new URL(u).searchParams.get('sourceIds')==='binance_exec_trade03'),combined_filter_sent:requests.some(u=>{const p=new URL(u).searchParams;return p.get('rowSymbol')==='CTKUSDT'&&p.get('rowStrategy')==='rbf_small'&&p.get('rowSort')==='price_shortfall'})}
 fs.writeFileSync(`${root}/result.json`,JSON.stringify(summary,null,2));console.log(JSON.stringify(summary))
} finally {await call('Network.clearBrowserCookies');ws.close()}
