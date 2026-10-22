import { useEffect, useState } from 'react'
import { getAccountStudio, listVirtualAccounts, saveAccountConfiguration } from '../api'
import type { AccountStudio, VirtualAccount } from '../types'
import { routes } from '../lib/routes'
import { useConfigWrite } from '../hooks/useConfigWrite'
import { Card, CardContent, CardHeader, CardTitle } from './ui/Card'
import { Input, Label, Select, FieldHint } from './ui/Field'
import { Button } from './ui/Button'
import { Alert } from './ui/Badge'

export function AccountFollowPanel({ sourceId, studio, onChange }: {
  sourceId: string
  studio: AccountStudio | null
  onChange: (studio: AccountStudio) => void
}) {
  const [accounts, setAccounts] = useState<VirtualAccount[]>([])
  const [mode, setMode] = useState('independent')
  const [follows, setFollows] = useState<{ virtual_id: string; multiplier: string }[]>([])
  const [readError, setReadError] = useState<string | null>(null)
  const { withWrite, saving, error, notice } = useConfigWrite()
  const config = studio?.configuration
  const savedFollows = JSON.stringify(config?.mode === 'follow' ? config.follows : [])
  const pending = studio?.pending_publishes ?? []
  useEffect(() => {
    setMode(config?.mode ?? 'independent')
    const saved = JSON.parse(savedFollows) as { virtual_id: string; multiplier: number }[]
    setFollows(saved.map((follow) => ({ ...follow, multiplier: String(follow.multiplier) })))
  }, [sourceId, config?.mode, savedFollows])
  useEffect(() => {
    const controller = new AbortController()
    listVirtualAccounts(controller.signal).then(setAccounts).catch((reason: unknown) => {
      if (reason instanceof DOMException && reason.name === 'AbortError') return
      setReadError(reason instanceof Error ? reason.message : String(reason))
    })
    return () => controller.abort()
  }, [sourceId])
  useEffect(() => {
    if (!sourceId || (config?.mode !== 'follow' && pending.length === 0)) return
    const controller = new AbortController()
    const timer = window.setInterval(() => {
      getAccountStudio(sourceId, controller.signal).then((next) => {
        onChange(next)
        setReadError(null)
      }).catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setReadError(reason instanceof Error ? reason.message : String(reason))
      })
    }, 5000)
    return () => { window.clearInterval(timer); controller.abort() }
  }, [sourceId, config?.mode, pending.length, onChange])
  const valid = mode === 'independent' || (follows.length > 0 && new Set(follows.map((follow) => follow.virtual_id)).size === follows.length && follows.every((follow) => follow.virtual_id && follow.multiplier.trim() !== '' && Number.isFinite(Number(follow.multiplier)) && Number(follow.multiplier) >= 0))
  return <Card>
    <CardHeader><CardTitle>账户配置模式</CardTitle></CardHeader>
    <CardContent className="space-y-4">
      {(error || readError) && <Alert tone="error">{error || readError}</Alert>}
      {notice && <Alert tone="success">{notice}</Alert>}
      <form onSubmit={(event) => {
        event.preventDefault()
        if (!valid) return
        void withWrite(async () => {
          const next = await saveAccountConfiguration(sourceId, mode === 'follow'
            ? { mode: 'follow', follows: follows.map((follow) => ({ ...follow, multiplier: Number(follow.multiplier) })) }
            : { mode: 'independent' })
          onChange(next)
          return next.pending_publishes.length ? '配置已保存，正在同步到 Exec。' : '配置模式已保存。'
        })
      }}>
        <fieldset disabled={saving || !studio} className="min-w-0 space-y-4">
          <Label className="max-w-sm">配置模式<Select value={mode} onChange={(event) => {
            setMode(event.target.value)
            if (event.target.value === 'follow' && follows.length === 0) setFollows([{ virtual_id: '', multiplier: '1' }])
          }}>
            <option value="independent">独立配置</option><option value="follow">跟随多个 Virtual</option>
          </Select></Label>
          {mode === 'follow' && <div className="space-y-3">
            {follows.map((follow, index) => <div key={index} className="grid min-w-0 items-end gap-3 rounded-xl border border-border p-3 sm:grid-cols-[minmax(0,1fr)_8rem_auto]">
              <Label className="min-w-0">Virtual 账户 {index + 1}<Select className="min-w-0" value={follow.virtual_id} onChange={(event) => setFollows((old) => old.map((item, i) => i === index ? { ...item, virtual_id: event.target.value } : item))}>
                <option value="">请选择</option>
                {follow.virtual_id && !accounts.some((a) => a.virtual_id === follow.virtual_id) && <option value={follow.virtual_id}>{follow.virtual_id}（无查看权限）</option>}
                {accounts.map((a) => <option key={a.virtual_id} value={a.virtual_id} disabled={follows.some((other, i) => i !== index && other.virtual_id === a.virtual_id)}>{a.virtual_id} · {a.name}</option>)}
              </Select></Label>
              <Label className="min-w-0">跟随倍率<Input value={follow.multiplier} inputMode="decimal" onChange={(event) => setFollows((old) => old.map((item, i) => i === index ? { ...item, multiplier: event.target.value } : item))} /></Label>
              <Button type="button" variant="ghost" aria-label={`移除 Virtual 账户 ${index + 1}`} onClick={() => setFollows((old) => old.filter((_, i) => i !== index))}>移除</Button>
            </div>)}
            <Button type="button" disabled={!accounts.some((a) => !follows.some((follow) => follow.virtual_id === a.virtual_id))} onClick={() => setFollows((old) => [...old, { virtual_id: '', multiplier: '1' }])}>添加 Virtual</Button>
            {follows.length === 0 && <FieldHint>请选择至少一个 Virtual；如需停止全部贡献，可将各倍率设为 0 后保存。</FieldHint>}
          </div>}
          <Button type="submit" variant="primary" disabled={!valid}>{saving ? '正在保存…' : '保存配置'}</Button>
        </fieldset>
      </form>
      <FieldHint>{mode === 'follow'
        ? '跟随会替换本账户的策略组合。各 Virtual 的份数 × 倍率后叠加；相同发布名的仓位策略与下单模板须一致。倍率 0 或移除只撤掉对应 Virtual 的贡献，其他贡献继续保留。'
        : '独立维护本账户的策略组合。从跟随模式切回独立配置时，会保留当前组合。'}</FieldHint>
      <a href={routes.virtualAccounts} className="text-sm text-brand">管理 Virtual 账户 →</a>
      {pending.length > 0 ? <Alert tone={'warning'}>
        待同步 {pending.length} 条，失败会自动重试。
        {pending.map((p) => <div key={p.binding_name}>{p.binding_name}{p.error ? `：${p.error}` : '：等待发布'}</div>)}
      </Alert> : config?.mode === 'follow' && <p className="text-sm text-muted">已同步。Virtual 后续修改将自动更新本账户。</p>}
    </CardContent>
  </Card>
}
