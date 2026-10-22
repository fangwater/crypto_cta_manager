import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'

// The short-lived cookie arrives over SSH stdin and is never written to disk.
const auth=JSON.parse(fs.readFileSync(0,'utf8'))
const stage=process.argv[2]
const version=await(await fetch('http://127.0.0.1:19344/json/version')).json()
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

try {
  for (const authenticated of [false, true]) {
    const context=await call('Target.createBrowserContext')
    try {
      const target=await call('Target.createTarget',{url:'about:blank',browserContextId:context.browserContextId})
      const {sessionId}=await call('Target.attachToTarget',{targetId:target.targetId,flatten:true})
      const c=(method,params)=>call(method,params,sessionId)
      const run=async expression=>{
        const r=await c('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true})
        if(r.exceptionDetails)throw new Error(JSON.stringify(r.exceptionDetails))
        return r.result.value
      }
      const waitFor=async expression=>{
        for(let n=0;n<240;n++){if(await run(expression))return;await pause(250)}
        throw new Error('auth recovery timed out: '+expression)
      }
      await c('Runtime.enable');await c('Network.enable');await c('Page.enable')
      if(authenticated)await c('Network.setCookie',{name:'cta_session',value:auth.token,url:base,httpOnly:true,sameSite:'Lax'})
      await c('Page.addScriptToEvaluateOnNewDocument',{source:`
        window.__authAttempts=0;
        const originalFetch=window.fetch;
        window.fetch=async(...args)=>{
          if(String(args[0]).includes('/api/auth/status')&&++window.__authAttempts===1)throw new TypeError('simulated temporary service outage');
          return originalFetch(...args);
        };
      `})
      const began=Date.now()
      await c('Page.navigate',{url:base+'/manager/accounts/'})
      await waitFor(`document.body?.textContent.includes('10 秒后自动重试')`)
      await waitFor(authenticated?`document.body?.textContent.includes('账户入口')`:`!!document.querySelector('input[type="password"]')`)
      assert.equal(await run(`document.body?.textContent.includes('认证服务不可用')`),false)
      const record=await run(`({attempts:window.__authAttempts,assets:[...document.scripts].map(s=>s.src).filter(Boolean)})`)
      assert.equal(record.attempts,2)
      results.push({authenticated,recovered:true,elapsedMs:Date.now()-began,...record})
    } finally {await call('Target.disposeBrowserContext',{browserContextId:context.browserContextId})}
  }
  console.log(JSON.stringify({passed:true,results}))
} finally {ws.close()}
