import fs from 'node:fs'
const root = '/home/fanghaizhou/snap/chromium/common/nav-visible-20261014'
const auth = JSON.parse(fs.readFileSync(`${root}/session.json`, 'utf8'))
const target = await (await fetch('http://127.0.0.1:19342/json/new?about:blank', {method:'PUT'})).json()
const ws = new WebSocket(target.webSocketDebuggerUrl)
await new Promise((resolve,reject)=>{ws.onopen=resolve;ws.onerror=reject})
let seq=0
const pending=new Map(), errors=[], requests=[], responses=[]
ws.onmessage=({data})=>{
 const msg=JSON.parse(data)
 if(msg.id){const p=pending.get(msg.id);if(p){pending.delete(msg.id);msg.error?p.reject(new Error(JSON.stringify(msg.error))):p.resolve(msg.result)}}
 else if(msg.method==='Runtime.exceptionThrown') errors.push(msg.params.exceptionDetails.text)
 else if(msg.method==='Network.requestWillBeSent' && /\/manager\/api\/(account-)?timeline\?/.test(msg.params.request.url)) requests.push(msg.params.request.url)
 else if(msg.method==='Network.responseReceived' && /\/manager\/(api\/|assets\/)/.test(msg.params.response.url)) responses.push({url:msg.params.response.url,status:msg.params.response.status,fromDiskCache:msg.params.response.fromDiskCache})
}
const call=(method,params={})=>new Promise((resolve,reject)=>{const id=++seq;pending.set(id,{resolve,reject});ws.send(JSON.stringify({id,method,params}))})
const run=async expression=>{const r=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(r.exceptionDetails)throw new Error(r.exceptionDetails.text);return r.result.value}
const pause=ms=>new Promise(resolve=>setTimeout(resolve,ms))
const summary=async()=>run(`(()=>({url:location.href,title:document.title,text:document.body.innerText.slice(0,16000),chartMode:Array.from(document.querySelectorAll('[aria-label="曲线视图"] button')).filter(b=>b.classList.contains('is-active')).map(b=>b.innerText),checkboxes:Array.from(document.querySelectorAll('.symbol-curve-picker label')).map(l=>({text:l.innerText,checked:l.querySelector('input')?.checked})),canvasCount:document.querySelectorAll('canvas').length,navResponses:window.__navResponses,strokes:window.__navStrokes,viewport:{width:innerWidth,height:innerHeight,overflow:document.documentElement.scrollWidth>innerWidth}}))()`)
try {
 await call('Network.enable');await call('Runtime.enable');await call('Page.enable')
 await call('Network.setCookie',{name:'cta_session',value:auth.token,url:'http://13.115.227.29:4191',httpOnly:true,sameSite:'Lax'})
 await call('Emulation.setTimezoneOverride',{timezoneId:'Asia/Shanghai'})
 await call('Emulation.setDeviceMetricsOverride',{width:1440,height:1000,deviceScaleFactor:1,mobile:false})
 await call('Page.addScriptToEvaluateOnNewDocument',{source:`
 window.__navResponses=[];window.__navStrokes={};
 const originalFetch=window.fetch;window.fetch=async(...args)=>{const result=await originalFetch(...args);if(/\\/manager\\/api\\/(account-)?timeline\\?/.test(result.url)){result.clone().json().then(value=>window.__navResponses.push({url:result.url,status:result.status,factual:value.report?.points?.length,theoretical:value.theoretical?.points?.length,loading:value.theoretical?.loading,reason:value.theoretical?.unavailable_reason})).catch(()=>{})}return result};
 const stroke=CanvasRenderingContext2D.prototype.stroke;CanvasRenderingContext2D.prototype.stroke=function(...args){window.__navStrokes[this.strokeStyle]=(window.__navStrokes[this.strokeStyle]||0)+1;return stroke.apply(this,args)};
 `})
 await call('Page.navigate',{url:'http://13.115.227.29:4191/manager/?source=binance_exec_trade03'})
 for(let n=0;n<60;n++) {
  await pause(1000)
  const ready=await run(`window.__navResponses?.some(r=>r.theoretical>0||r.reason&&!r.loading)`)
  if(ready)break
 }
 await pause(1000)
 const result=await summary();result.errors=errors;result.requests=requests;result.responses=responses
 fs.writeFileSync(`${root}/result.json`,JSON.stringify(result,null,2))
 const screenshot=await call('Page.captureScreenshot',{format:'png',captureBeyondViewport:true})
 fs.writeFileSync(`${root}/desktop.png`,Buffer.from(screenshot.data,'base64'))
 console.log(JSON.stringify(result))
} finally {ws.close()}
