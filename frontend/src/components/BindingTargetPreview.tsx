import { useEffect, useState } from 'react'
import { previewAccountBindingTargets } from '../api'
import type { BindingTargetPreview as Preview } from '../types'
import { Alert } from './ui/Badge'
import { Button } from './ui/Button'

function filterReason(reason: string) {
  if (reason === 'not a USDT perpetual target') return '不属于 USDT 永续'
  if (reason === 'symbol absent from Bybit cache') return 'Bybit 无对应合约'
  if (reason.startsWith('Bybit contract is not tradable:')) return `Bybit 合约不可交易（${reason.split(': ')[1]}）`
  return reason
}

export function BindingTargetPreview({ sourceId, bindingName }: { sourceId: string; bindingName: string }) {
  const [revision, setRevision] = useState(0)
  const [preview, setPreview] = useState<Preview | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  useEffect(() => {
    if (!revision) return
    const controller = new AbortController()
    setLoading(true)
    setPreview(null)
    setError(null)
    previewAccountBindingTargets(sourceId, bindingName, controller.signal)
      .then(setPreview)
      .catch((reason: unknown) => {
        if (controller.signal.aborted) return
        setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [sourceId, bindingName, revision])

  return <div className="space-y-3 border-t border-border pt-3">
    <Button type="button" size="sm" disabled={loading} onClick={() => setRevision((value) => value + 1)}>
      {loading ? '正在检查目标…' : '预览发布目标'}
    </Button>
    {error && <Alert tone="warning">{error}</Alert>}
    {preview && <div className="space-y-2 text-sm">
      <p className="text-muted">
        {preview.proxy_enabled ? 'Bybit 过滤与单位换算' : '目标数量预览'}：{Object.keys(preview.targets).length} 个合约，份数 {preview.shares}。
        {preview.projection && ` 已过滤 ${Object.keys(preview.projection.filtered).length} 个 symbol。`}
        本次仅预览；重推时会重新检查最新规则和目标。
      </p>
      {preview.projection && <p className="text-xs text-muted">合约规则更新时间：{new Date(preview.projection.rules_fetched_at_us / 1000).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai' })}（上海时间）</p>}
      <div className="max-h-72 overflow-auto">
        <table className="w-full text-left text-xs">
          <thead><tr><th className="p-2">输入 symbol</th><th className="p-2">执行 symbol</th><th className="p-2">单位系数</th><th className="p-2">发布 qty</th><th className="p-2">signal</th></tr></thead>
          <tbody>{Object.entries(preview.projection?.matched ?? Object.fromEntries(Object.keys(preview.targets).map((symbol) => [symbol, { symbol, quantity_multiplier: 1 }]))).map(([input, mapping]) => <tr key={input} className="border-t border-border">
            <td className="p-2">{input}</td><td className="p-2">{mapping.symbol}</td><td className="p-2">{mapping.quantity_multiplier}</td>
            <td className="p-2 font-mono">{preview.targets[mapping.symbol]?.qty}</td><td className="p-2">{preview.targets[mapping.symbol]?.signal}</td>
          </tr>)}</tbody>
        </table>
      </div>
      {preview.projection && Object.keys(preview.projection.filtered).length > 0 && <ul className="space-y-1 text-muted">
        {Object.entries(preview.projection.filtered).map(([symbol, reason]) => <li key={symbol}>{symbol}：{filterReason(reason)}</li>)}
      </ul>}
    </div>}
  </div>
}
