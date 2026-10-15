import fs from 'node:fs'
const auth = JSON.parse(fs.readFileSync(0, 'utf8'))
const target = await (await fetch('http://127.0.0.1:19357/json/new?about:blank', {method: 'PUT'})).json()
const ws = new WebSocket(target.webSocketDebuggerUrl)
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject })
let seq = 0
const pending = new Map(), errors = [], failed = []
ws.onmessage = ({data}) => {
  const msg = JSON.parse(data)
  if (msg.id) {
    const task = pending.get(msg.id)
    if (task) { pending.delete(msg.id); msg.error ? task.reject(new Error(JSON.stringify(msg.error))) : task.resolve(msg.result) }
  } else if (msg.method === 'Runtime.exceptionThrown') errors.push(msg.params.exceptionDetails.text)
  else if (msg.method === 'Network.responseReceived' && msg.params.response.status >= 400 && msg.params.response.url.includes('/manager/')) failed.push(msg.params.response.status)
}
const call = (method, params = {}) => new Promise((resolve, reject) => { const id = ++seq; pending.set(id, {resolve, reject}); ws.send(JSON.stringify({id, method, params})) })
const run = async expression => {
  const response = await call('Runtime.evaluate', {expression, returnByValue: true, awaitPromise: true})
  if (response.exceptionDetails) throw new Error(response.exceptionDetails.text)
  return response.result.value
}
const pause = ms => new Promise(resolve => setTimeout(resolve, ms))
try {
  await call('Network.enable'); await call('Runtime.enable'); await call('Page.enable')
  await call('Network.setCookie', {name: 'cta_session', value: auth.token, url: 'http://13.115.227.29:4191', httpOnly: true, sameSite: 'Lax'})
  await call('Page.addScriptToEvaluateOnNewDocument', {source: `
    window.__dashboard = null;
    const original = window.fetch;
    window.fetch = async (...args) => {
      const response = await original(...args);
      if (response.url.endsWith('/api/dashboard')) response.clone().json().then(row => { window.__dashboard = row; });
      return response;
    };
  `})
  const results = []
  for (const [width, height] of [[1440, 1000], [390, 1100]]) {
    await call('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 1, mobile: width < 500})
    await call('Page.navigate', {url: 'http://13.115.227.29:4191/manager/workspace/'})
    let result = null
    for (let attempt = 0; attempt < 40; attempt++) {
      await pause(500)
      result = await run(`(() => {
        const account = window.__dashboard?.accounts?.find(row => row.source_id === 'binance_exec_trade06');
        const title = Array.from(document.querySelectorAll('h3')).find(row => row.textContent === 'lgq_cta_RX01');
        const card = title?.closest('div.rounded-2xl');
        const label = Array.from(card?.querySelectorAll('p') || []).find(row => row.textContent === '实时权益');
        return {label: label?.nextElementSibling?.textContent, equity: account?.live_equity_usdt,
          status: account?.live_equity_status, text: card?.innerText,
          overflow: document.documentElement.scrollWidth > innerWidth};
      })()`)
      if (result.equity != null && result.label?.includes('USDT')) break
    }
    if (!result || result.status !== 'ok' || !(result.equity > 0) || !result.label?.includes('USDT') || result.overflow) throw new Error('Equity card failed: ' + JSON.stringify(result))
    const displayed = Number(result.label.replace(/[\s,]|USDT/g, ''))
    if (Math.abs(displayed - result.equity) > 0.0051) throw new Error('Displayed equity does not match dashboard')
    results.push({width, height, displayed: result.label, equity: result.equity, status: result.status, horizontal_overflow: result.overflow})
  }
  if (errors.length || failed.length) throw new Error('Browser errors: ' + JSON.stringify({errors, failed}))
  console.log(JSON.stringify({passed: true, username: 'lgq', results, javascript_errors: errors, failed_responses: failed}))
} finally {
  await call('Network.clearBrowserCookies')
  ws.close()
}
