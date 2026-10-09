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
  const [virtualId, setVirtualId] = useState('')
  const [multiplier, setMultiplier] = useState('1')
  const [readError, setReadError] = useState<string | null>(null)
  const { withWrite, saving, error, notice } = useConfigWrite()
  const config = studio?.configuration
  const followedId = config?.mode === 'follow' ? config.virtual_id : ''
  const followedMultiplier = config?.mode === 'follow' ? config.multiplier : 1
  const pending = studio?.pending_publishes ?? []
  useEffect(() => {
    setMode(config?.mode ?? 'independent')
    setVirtualId(followedId)
    setMultiplier(String(followedMultiplier))
  }, [sourceId, config?.mode, followedId, followedMultiplier])
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
  const numericMultiplier = Number(multiplier)
  const valid = mode === 'independent' || (!!virtualId && multiplier.trim() !== '' && Number.isFinite(numericMultiplier) && numericMultiplier >= 0)
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
            ? { mode: 'follow', virtual_id: virtualId, multiplier: numericMultiplier }
            : { mode: 'independent' })
          onChange(next)
          return next.pending_publishes.length ? '配置已保存，正在同步到 Exec。' : '配置模式已保存。'
        })
      }}>
        <fieldset disabled={saving || !studio} className={mode === 'follow'
          ? 'grid min-w-0 items-end gap-4 sm:grid-cols-2 xl:grid-cols-[12rem_minmax(0,1fr)_8rem_auto]'
          : 'grid min-w-0 items-end gap-4 sm:grid-cols-[minmax(0,20rem)_auto] sm:justify-start'}>
        <Label className="min-w-0">配置模式<Select value={mode} onChange={(event) => setMode(event.target.value)}>
          <option value="independent">独立配置</option><option value="follow">跟随 Virtual</option>
        </Select></Label>
        {mode === 'follow' && <>
          <Label className="min-w-0">Virtual 账户<Select className="min-w-0" value={virtualId} onChange={(event) => setVirtualId(event.target.value)}>
            <option value="">请选择</option>
            {followedId && !accounts.some((a) => a.virtual_id === followedId) && <option value={followedId}>{followedId}（无查看权限）</option>}
            {accounts.map((a) => <option key={a.virtual_id} value={a.virtual_id}>{a.virtual_id} · {a.name}</option>)}
          </Select></Label>
          <Label className="min-w-0">跟随倍率<Input value={multiplier} inputMode="decimal" onChange={(event) => setMultiplier(event.target.value)} /></Label>
        </>}
        <Button type="submit" variant="primary" disabled={!valid}>{saving ? '正在保存…' : '保存模式'}</Button>
        </fieldset>
      </form>
      <FieldHint>{mode === 'follow'
        ? '跟随会替换本账户的策略组合。生效份数 = Virtual 份数 × 跟随倍率；倍率 0 停止全部策略，移除的策略也会同步停止。'
        : '独立维护本账户的策略组合。从跟随模式切回独立配置时，会保留当前组合。'}</FieldHint>
      <a href={routes.virtualAccounts} className="text-sm text-brand">管理 Virtual 账户 →</a>
      {pending.length > 0 ? <Alert tone={'warning'}>
        待同步 {pending.length} 条，失败会自动重试。
        {pending.map((p) => <div key={p.binding_name}>{p.binding_name}{p.error ? `：${p.error}` : '：等待发布'}</div>)}
      </Alert> : config?.mode === 'follow' && <p className="text-sm text-muted">已同步。Virtual 后续修改将自动更新本账户。</p>}
    </CardContent>
  </Card>
}
