import { Check, CirclePlay, PiggyBank, RefreshCw, RotateCcw } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  getAutoEarn,
  getDashboard,
  resumeAutoEarn,
  runAutoEarn,
  saveAutoEarn,
  type AutoEarnSettings,
} from '../api'
import { AppShell, PageIntro } from '../components/AppShell'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Input, Label, Select } from '../components/ui/Field'
import { readSourceId } from '../lib/routes'
import type { DashboardAccount } from '../types'

const defaults: AutoEarnSettings = {
  enabled: false,
  interval_secs: 180,
  round_cap_usdt: 5000,
  trigger_usdt: 50,
  paused: false,
  running: false,
  last_result: null,
}

export function AutoEarnPage() {
  const [accounts, setAccounts] = useState<DashboardAccount[]>([])
  const [sourceId, setSourceId] = useState(readSourceId)
  const [settings, setSettings] = useState<AutoEarnSettings>(defaults)
  const [token, setToken] = useState('')
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')

  useEffect(() => {
    let live = true
    void getDashboard()
      .then((dashboard) => {
        if (!live) return
        const available = (dashboard.accounts ?? []).filter(
          (account) => account.enabled && account.venue === 'binance-futures',
        )
        setAccounts(available)
        if (!available.some((account) => account.source_id === sourceId)) {
          setSourceId(available[0]?.source_id ?? '')
        }
      })
      .catch((reason) => live && setError(String(reason)))
      .finally(() => live && setLoading(false))
    return () => { live = false }
  }, [])

  const refresh = useCallback(async () => {
    if (!sourceId) return
    try {
      setSettings(await getAutoEarn(sourceId))
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason))
    }
  }, [sourceId])

  useEffect(() => {
    setSettings(defaults)
    setError('')
    setNotice('')
    setToken('')
    void refresh()
  }, [refresh])

  const selected = useMemo(
    () => accounts.find((account) => account.source_id === sourceId),
    [accounts, sourceId],
  )
  const configurable = selected?.configurable ?? false

  async function act(action: 'save' | 'run' | 'resume') {
    if (!sourceId || !token.trim()) {
      setError('请输入自动理财操作 token')
      return
    }
    setBusy(true)
    setError('')
    setNotice('')
    try {
      if (action === 'save') {
        setSettings(await saveAutoEarn(sourceId, settings, token))
        setNotice('设置已保存')
      } else if (action === 'resume') {
        setSettings(await resumeAutoEarn(sourceId, token))
        setNotice('暂停已解除')
      } else {
        const response = await runAutoEarn(sourceId, token)
        setNotice(response.result)
        await refresh()
      }
      setToken('')
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason)
      await refresh()
      setError(message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <AppShell active="auto-earn" title="自动理财" subtitle="Binance STANDARD" icon={PiggyBank}>
      <PageIntro eyebrow="Account" title="自动理财" actions={
        <Button type="button" variant="ghost" onClick={() => void refresh()} title="刷新状态" disabled={!sourceId}>
          <RefreshCw size={16} />
        </Button>
      } />
      {error && <Alert tone="error" className="mb-5">{error}</Alert>}
      {notice && <Alert tone="success" className="mb-5">{notice}</Alert>}
      <div className="max-w-3xl space-y-6">
        <div className="flex flex-wrap items-end gap-4 border-b border-border pb-5">
          <Label className="min-w-56 flex-1">账户
            <Select value={sourceId} onChange={(event) => setSourceId(event.target.value)} disabled={loading || busy}>
              {accounts.map((account) => <option key={account.source_id} value={account.source_id}>{account.account}</option>)}
            </Select>
          </Label>
          <Badge tone={settings.paused ? 'warning' : settings.enabled ? 'success' : 'neutral'}>
            {settings.running ? '执行中' : settings.paused ? '已暂停' : settings.enabled ? '已启用' : '已关闭'}
          </Badge>
        </div>
        {!loading && !sourceId && <Alert tone="warning">没有可用的 Binance 账户</Alert>}
        {sourceId && <>
          {settings.paused && <Alert tone="warning">上次执行可能只完成了部分步骤。核对现货和合约余额后再解除暂停。</Alert>}
          {settings.last_result && <p className="text-sm text-muted">最近结果：{settings.last_result}</p>}
          <div className="grid gap-5 border-b border-border pb-6 sm:grid-cols-2">
            <Label className="sm:col-span-2">
              <span className="flex items-center gap-3 text-sm text-ink">
                <input type="checkbox" checked={settings.enabled} disabled={!configurable || busy}
                  onChange={(event) => setSettings({ ...settings, enabled: event.target.checked })} />
                启用自动申购
              </span>
            </Label>
            <Label>执行间隔（分钟）
              <Input type="number" min="1" max="1440" step="1" value={settings.interval_secs / 60}
                disabled={!configurable || busy}
                onChange={(event) => setSettings({ ...settings, interval_secs: Number(event.target.value) * 60 })} />
            </Label>
            <Label>每轮上限（USDT）
              <Input type="number" min="1" max="1000000" step="0.01" value={settings.round_cap_usdt}
                disabled={!configurable || busy}
                onChange={(event) => setSettings({ ...settings, round_cap_usdt: Number(event.target.value) })} />
            </Label>
            <Label>触发下限（USDT）
              <Input type="number" min="0" max="1000000" step="0.01" value={settings.trigger_usdt}
                disabled={!configurable || busy}
                onChange={(event) => setSettings({ ...settings, trigger_usdt: Number(event.target.value) })} />
            </Label>
          </div>
          {configurable && <div className="space-y-4">
            <Label>操作 token
              <Input type="password" autoComplete="off" value={token} onChange={(event) => setToken(event.target.value)} />
            </Label>
            <div className="flex flex-wrap gap-2">
              <Button variant="primary" disabled={busy || !token} onClick={() => void act('save')}>
                <Check size={16} /> 保存
              </Button>
              <Button disabled={busy || !token || !settings.enabled || settings.paused} onClick={() => void act('run')}>
                <CirclePlay size={16} /> 立即执行
              </Button>
              {settings.paused && <Button variant="secondary" disabled={busy || !token} onClick={() => void act('resume')}>
                <RotateCcw size={16} /> 解除暂停
              </Button>}
            </div>
          </div>}
        </>}
      </div>
    </AppShell>
  )
}
