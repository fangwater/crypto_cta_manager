import { History, LoaderCircle, RefreshCw } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { getAccountSharesHistory } from '../api'
import type { AccountStudio, DashboardAccount, SharesHistory } from '../types'
import { accountVenueLabel } from '../lib/workspace'
import { Alert } from './ui/Badge'
import { Button } from './ui/Button'
import { Card, CardContent } from './ui/Card'

const time = new Intl.DateTimeFormat('zh-CN', {
  timeZone: 'Asia/Shanghai', year: 'numeric', month: '2-digit', day: '2-digit',
  hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23',
})
const origins: Record<string, string> = { manual: '手动调整', virtual: 'Virtual 组合调整', follow: '跟随设置调整', external: '外部修改' }

export function AccountStrategySharesHistory({ sourceId, revision, markets = [] }: {
  sourceId: string
  revision: AccountStudio | AccountStudio[] | null
  markets?: Pick<DashboardAccount, 'source_id' | 'venue'>[]
}) {
  const [open, setOpen] = useState(false)
  const [reload, setReload] = useState(0)
  const [history, setHistory] = useState<SharesHistory | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const active = useRef<AbortController | null>(null)

  useEffect(() => {
    setHistory(null)
    setError(null)
    if (!open || !sourceId) return
    const controller = new AbortController()
    active.current = controller
    setLoading(true)
    getAccountSharesHistory(sourceId, null, controller.signal)
      .then(next => { if (!controller.signal.aborted) setHistory(next) })
      .catch((reason: unknown) => {
        if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [sourceId, revision, open, reload])

  async function loadOlder() {
    const controller = active.current
    if (!controller || controller.signal.aborted || !history?.next_cursor || loading) return
    setLoading(true)
    setError(null)
    try {
      const next = await getAccountSharesHistory(sourceId, history.next_cursor, controller.signal)
      if (!controller.signal.aborted) {
        setHistory(previous => previous && {
          current: next.current,
          entries: [...previous.entries, ...next.entries.filter(entry => !previous.entries.some(old => old.change_id === entry.change_id))],
          next_cursor: next.next_cursor,
        })
      }
    } catch (reason) {
      if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      if (!controller.signal.aborted) setLoading(false)
    }
  }

  const marketLabel = (id: string) => {
    const market = markets.find(entry => entry.source_id === id)
    return market ? accountVenueLabel(market.venue) : id
  }

  return <Card>
    <details open={open} onToggle={event => setOpen(event.currentTarget.open)}>
      <summary className="cursor-pointer px-5 py-4 text-sm font-medium">
        <History size={16} className="mr-2 inline-block text-brand" />账户策略份数记录
      </summary>
      {open && <CardContent className="space-y-3">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <p className="max-w-3xl text-xs leading-6 text-muted">
            按本账户的底层实际策略汇总份数；Virtual 按各层配比和账户跟随倍率展开，同一策略的贡献相加。
            同一账户、同一策略相邻变化不超过 1 分钟时合并，改回原份数则隐藏。
            记录从功能启用后开始长期保留。时间为上海时间，表示配置保存时间；正数份数在下次仓位推送或手动重推时应用。
          </p>
          <Button type="button" variant="ghost" disabled={loading} onClick={() => setReload(value => value + 1)}>
            <RefreshCw size={14} />刷新
          </Button>
        </div>
        {error && <Alert tone="error">{error}</Alert>}
        {history && <div>
          <p className="mb-2 text-sm font-medium">当前配置份数</p>
          {history.current.length === 0 ? <p className="text-sm text-muted">当前未配置正数份数的策略。</p> : <div className="overflow-x-auto">
            <table className="w-full text-left text-xs">
              <thead className="text-muted"><tr><th className="p-2">实际策略</th>{markets.length > 1 && <th className="p-2">市场</th>}<th className="p-2">合计份数</th></tr></thead>
              <tbody>{history.current.map(entry => <tr key={`${entry.source_id}:${entry.strategy_name}`} className="border-t border-border-soft">
                <td className="break-all p-2">{entry.strategy_name}</td>{markets.length > 1 && <td className="p-2">{marketLabel(entry.source_id)}</td>}<td className="p-2 tabular-nums">{entry.shares}</td>
              </tr>)}</tbody>
            </table>
          </div>}
          <p className="mb-2 mt-5 text-sm font-medium">份数变化</p>
        </div>}
        {history && history.entries.length === 0 && <p className="py-5 text-center text-sm text-muted">暂无份数变更记录。</p>}
        {!!history?.entries.length && <div className="overflow-x-auto">
          <table className="w-full min-w-[660px] text-left text-xs">
            <thead className="text-muted"><tr>
              <th className="p-2">实际策略</th>{markets.length > 1 && <th className="p-2">市场</th>}<th className="p-2">合计份数变化</th><th className="p-2">保存时间</th><th className="p-2">操作人</th><th className="p-2">来源</th>
            </tr></thead>
            <tbody>{history.entries.map(entry => <tr key={entry.change_id} className="border-t border-border-soft align-top">
              <td className="max-w-56 break-all p-2">{entry.strategy_name}</td>
              {markets.length > 1 && <td className="p-2">{marketLabel(entry.source_id)}</td>}
              <td className="whitespace-nowrap p-2 tabular-nums">{entry.initial_shares} → {entry.final_shares}
                {entry.change_count > 1 && <span className="block text-muted">合并 {entry.change_count} 次调整</span>}
              </td>
              <td className="whitespace-nowrap p-2 tabular-nums">
                {time.format(entry.first_changed_at_us / 1000)}
                {entry.last_changed_at_us !== entry.first_changed_at_us && <span className="block text-muted">至 {time.format(entry.last_changed_at_us / 1000)}</span>}
              </td>
              <td className="max-w-40 break-all p-2">{entry.actors.map(actor => actor.username).join('、') || '未记录'}</td>
              <td className="p-2">{entry.origins.map(origin => origins[origin] ?? origin).join('、')}</td>
            </tr>)}</tbody>
          </table>
        </div>}
        {loading && <p role="status" className="flex items-center justify-center gap-2 py-3 text-sm text-muted"><LoaderCircle size={16} className="animate-spin" />正在加载</p>}
        {history?.next_cursor && <Button type="button" variant="ghost" disabled={loading} onClick={() => void loadOlder()}>加载更早记录</Button>}
      </CardContent>}
    </details>
  </Card>
}
