import { useEffect, useRef, useState } from 'react'
import { getAccountFunding, saveAccountFunding } from '../api'
import { timestampUs } from '../format'
import type { FundingSettings } from '../types'
import { FundingStatusBadge } from './FundingStatusBadge'
import { Alert } from './ui/Badge'
import { Button } from './ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/Card'
import { FieldHint, Input, Label } from './ui/Field'

export function FundingSettingsPanel({ sourceId, canConfigure = true }: { sourceId: string; canConfigure?: boolean }) {
  const [settings, setSettings] = useState<FundingSettings | null>(null)
  const [interval, setInterval] = useState('300')
  const [token, setToken] = useState('')
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const writeController = useRef<AbortController | null>(null)
  const busy = useRef(false)

  useEffect(() => {
    const controller = new AbortController()
    let initialized = false
    let timer: ReturnType<typeof setTimeout> | undefined
    setSettings(null)
    setToken('')
    setSaving(false)
    setError(null)
    setNotice(null)
    const load = async () => {
      try {
        const value = await getAccountFunding(sourceId, controller.signal)
        if (!controller.signal.aborted && !busy.current) {
          setSettings(value)
          if (!initialized) setInterval(String(value.refresh_secs))
          initialized = true
        }
      } catch (reason) {
        if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : '无法加载 funding 配置')
      } finally {
        if (!controller.signal.aborted) timer = setTimeout(() => void load(), 15000)
      }
    }
    if (sourceId) void load()
    return () => {
      controller.abort()
      writeController.current?.abort()
      busy.current = false
      if (timer) clearTimeout(timer)
    }
  }, [sourceId])

  async function save(enabled: boolean) {
    if (!settings || busy.current) return
    const seconds = Number(interval)
    if (!Number.isInteger(seconds) || seconds < 60 || seconds > 86400) {
      setError('拉取间隔必须为 60–86400 秒的整数')
      return
    }
    const controller = new AbortController()
    writeController.current = controller
    busy.current = true
    setSaving(true)
    setError(null)
    setNotice(null)
    try {
      const value = await saveAccountFunding(sourceId, enabled, seconds, enabled && !settings.enabled ? token : undefined, controller.signal)
      if (!controller.signal.aborted) {
        setSettings(value)
        setInterval(String(value.refresh_secs))
        setToken('')
        setNotice(enabled && !settings.enabled ? '固定出口与账户认证测试通过，已开启历史补齐。' : enabled ? '拉取间隔已保存并生效。' : '已暂停采集，历史账单已保留，净值页总体盈亏改用估算 Funding。')
      }
    } catch (reason) {
      if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : '保存失败')
    } finally {
      if (!controller.signal.aborted) { busy.current = false; setSaving(false) }
    }
  }

  return <Card>
    <CardHeader className="flex flex-wrap items-start justify-between gap-3">
      <div><CardTitle>Funding 记录</CardTitle><CardDescription>从本机最早订单开始补齐实际资金费。净值页总体盈亏在开启时使用实际 Funding，关闭时使用估算 Funding。</CardDescription></div>
      {settings && <FundingStatusBadge value={settings.summary} />}
    </CardHeader>
    <CardContent className="space-y-4">
      {error && <Alert>{error}</Alert>}
      {notice && <Alert tone="success">{notice}</Alert>}
      {!settings ? <p className="text-sm text-muted">加载配置中…</p> : <>
        <p className="text-sm text-muted break-words">固定出口 {settings.local_ip ?? '未配置'} · 公网 IP {settings.public_ip ?? '未配置'}</p>
        {settings.unavailable_reason && <Alert tone="warning">{settings.unavailable_reason}</Alert>}
        <div className="grid gap-4 sm:grid-cols-2">
          <Label>拉取间隔（秒）<Input type="number" min="60" max="86400" step="1" value={interval} disabled={!canConfigure || saving} onChange={event => setInterval(event.target.value)} /><FieldHint>默认 300 秒。历史补齐会连续推进，完成后按此间隔增量拉取。</FieldHint></Label>
          {canConfigure && !settings.enabled && <Label>启用 Token<Input type="password" autoComplete="off" value={token} disabled={saving} onChange={event => setToken(event.target.value)} /><FieldHint>每次开启需要 token，并实测固定出口及账户 REST 权限。</FieldHint></Label>}
        </div>
        {canConfigure && <div className="flex flex-wrap gap-2">
          {settings.enabled ? <>
            <Button variant="primary" disabled={saving || Number(interval) === settings.refresh_secs} onClick={() => void save(true)}>保存拉取间隔</Button>
            <Button disabled={saving} onClick={() => void save(false)}>暂停采集</Button>
          </> : <Button variant="primary" disabled={saving || !token.trim() || !!settings.unavailable_reason} onClick={() => void save(true)}>{saving ? '测试固定出口与账户权限…' : '测试出口并开启'}</Button>}
        </div>}
        <div className="space-y-1 text-xs text-muted">
          <p>最近通过测试：{settings.verified_at_us == null ? '尚未测试' : timestampUs(settings.verified_at_us)}</p>
          <p>历史起点：{settings.summary.start_ts_us == null ? '开启后从本机订单读取' : timestampUs(settings.summary.start_ts_us)}</p>
          <p>已检查至：{settings.summary.synced_through_ts_us == null ? '等待拉取' : timestampUs(settings.summary.synced_through_ts_us)} · {settings.summary.symbols.length} 个币种 · {settings.summary.record_count} 笔账单</p>
          {settings.summary.last_error && <p role="status">{settings.summary.last_error}</p>}
        </div>
      </>}
    </CardContent>
  </Card>
}
