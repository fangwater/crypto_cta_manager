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
async function scenario(username,width,height) {
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
      for(let n=0;n<180;n++) {if(await run(expression))return;await pause(250)}
      throw new Error('browser condition timed out')
    }
    await c('Runtime.enable');await c('Network.enable');await c('Page.enable')
    await c('Network.setCookie',{name:'cta_session',value:auth[username].token,url:'http://172.16.30.42:10041',httpOnly:true,sameSite:'Lax'})
    await c('Emulation.setDeviceMetricsOverride',{width,height,deviceScaleFactor:1,mobile:width<600})
    await c('Page.navigate',{url:'http://172.16.30.42:10041/manager/admin/'})
    await waitFor(`document.body?.textContent?.includes('CTA_SK_C40V5D3Pos_filter')`)
    const permissions=await run(`(()=>({overflow:document.documentElement.scrollWidth>innerWidth,
      ownedVisible:document.body.textContent.includes('CTA_SK_C40V5D3Pos_filter'),
      description:document.body.textContent.includes('创建者和推送管理者自动拥有策略配置权限'),
      implicitRows:[...document.querySelectorAll('span')].filter(e=>e.textContent==='shaokai · 创建者（自动可配置）').map(e=>({
        buttons:[...e.parentElement.querySelectorAll('button')].map(b=>({text:b.textContent,disabled:b.disabled,selected:b.classList.contains('bg-brand')}))})),
      alerts:[...document.querySelectorAll('[role="alert"]')].map(e=>e.textContent)}))()`)
    assert.ok(permissions.ownedVisible);assert.ok(permissions.description)
    assert.equal(permissions.alerts.length,0)
    if(username==='admin') {
      assert.equal(permissions.implicitRows.length,10)
      assert.ok(permissions.implicitRows.every(r=>r.buttons.every(b=>b.disabled)&&r.buttons.find(b=>b.text==='可配置')?.selected))
    }
    const screen=await c('Page.captureScreenshot',{format:'png',captureBeyondViewport:false})
    fs.writeFileSync(path.join(stage,`${username}_${width}.png`),Buffer.from(screen.data,'base64'))
    let bindings=null
    if(username==='shaokai') {
      await c('Page.navigate',{url:'http://172.16.30.42:10041/manager/config/bindings/?source=binance_exec_trade01'})
      await waitFor(`[...document.querySelectorAll('select option')].some(e=>e.value==='CTA_SK_C40V5D3Pos_filter')`)
      bindings=await run(`(()=>({overflow:document.documentElement.scrollWidth>innerWidth,
        ownedOption:[...document.querySelectorAll('select option')].some(e=>e.value==='CTA_SK_C40V5D3Pos_filter'),
        alerts:[...document.querySelectorAll('[role="alert"]')].map(e=>e.textContent)}))()`)
      assert.ok(bindings.ownedOption);assert.equal(bindings.alerts.length,0)
      await c('Page.navigate',{url:'http://172.16.30.42:10041/manager/config/position/'})
      await waitFor(`document.body?.textContent?.includes('CTA_SK_C40V5D3Pos_filter')`)
      await run(`[...document.querySelectorAll('button')].find(e=>e.querySelector('p')?.textContent==='CTA_SK_C40V5D3Pos_filter').click()`)
      assert.ok(await run(`[...document.querySelectorAll('button')].some(e=>e.textContent.includes('保存')&&!e.disabled)`))
    }
    assert.equal(state.errors.length,0)
    results.push({username,width,height,permissions,bindings,jsErrors:state.errors,readOnly:true})
    sessions.delete(sessionId)
  } finally {await call('Target.disposeBrowserContext',context)}
}
try {
  for(const username of ['shaokai','admin']) {
    await scenario(username,1440,1000);await scenario(username,390,1100)
  }
  console.log(JSON.stringify({passed:true,results},null,2))
} finally {ws.close()}
