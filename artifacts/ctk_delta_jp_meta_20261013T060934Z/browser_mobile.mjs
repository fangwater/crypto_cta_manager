import fs from 'node:fs'
const root='/home/fanghaizhou/snap/chromium/common/ctk-deployment-20261013T060934Z'
const pages=await(await fetch('http://127.0.0.1:19334/json/list')).json()
const page=pages.find(p=>p.url.includes('/manager/acquisition-cost/'))
const ws=new WebSocket(page.webSocketDebuggerUrl);await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j});let seq=0;const pending=new Map()
ws.onmessage=({data})=>{const m=JSON.parse(data);if(m.id&&pending.has(m.id)){const p=pending.get(m.id);pending.delete(m.id);m.error?p.j(m.error):p.r(m.result)}}
const call=(method,params={})=>new Promise((r,j)=>{const id=++seq;pending.set(id,{r,j});ws.send(JSON.stringify({id,method,params}))})
await call('Emulation.setDeviceMetricsOverride',{width:390,height:1100,deviceScaleFactor:1,mobile:true})
await new Promise(r=>setTimeout(r,500))
await call('Runtime.evaluate',{expression:"document.getElementById('delta-details').scrollIntoView(); window.scrollBy(0,-130)"})
await new Promise(r=>setTimeout(r,500))
const shot=await call('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});fs.writeFileSync(root+'/mobile.png',Buffer.from(shot.data,'base64'))
ws.close();console.log('Mobile detail screenshot captured')
