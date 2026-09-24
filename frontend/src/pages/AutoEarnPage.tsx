import {
  Check,
  CirclePlay,
  Clock3,
  Eye,
  EyeOff,
  LoaderCircle,
  PiggyBank,
  RefreshCw,
  RotateCcw,
  Settings2,
} from 'lucide-react'
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
  interval_secs: 3600,
  round_cap_usdt: 5000,
  trigger_usdt: 100,
  paused: false,
  running: false,
  last_result: null,
}

export function AutoEarnPage() {
  const [accounts, setAccounts] = useState<DashboardAccount[]>([])
  const [sourceId, setSourceId] = useState(readSourceId)
  const [settings, setSettings] = useState<AutoEarnSettings>(defaults)
  const [savedSettings, setSavedSettings] = useState<AutoEarnSettings | null>(null)
  const [token, setToken] = useState('')
  const [tokenVisible, setTokenVisible] = useState(false)
  const [loading, setLoading] = useState(true)
  const [settingsLoading, setSettingsLoading] = useState(false)
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

  const refresh = useCallback(async (signal?: AbortSignal) => {
    if (!sourceId) return
    setSettingsLoading(true)
    try {
      const next = await getAutoEarn(sourceId, signal)
      setSettings(next)
      setSavedSettings(next)
      setError('')
    } catch (reason) {
      if (reason instanceof DOMException && reason.name === 'AbortError') return
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      if (!signal?.aborted) setSettingsLoading(false)
    }
  }, [sourceId])

  useEffect(() => {
    const controller = new AbortController()
    setSettings(defaults)
    setSavedSettings(null)
    setError('')
    setNotice('')
    setToken('')
    setTokenVisible(false)
    void refresh(controller.signal)
    return () => controller.abort()
  }, [refresh])

  const selected = useMemo(
    () => accounts.find((account) => account.source_id === sourceId),
    [accounts, sourceId],
  )
  const configurable = selected?.configurable ?? false
  const dirty = savedSettings !== null && (
    settings.enabled !== savedSettings.enabled ||
    settings.interval_secs !== savedSettings.interval_secs ||
    settings.round_cap_usdt !== savedSettings.round_cap_usdt ||
    settings.trigger_usdt !== savedSettings.trigger_usdt
  )
  const statusLabel = settingsLoading && !savedSettings
    ? '读取中'
    : !savedSettings ? '不可用'
      : settings.running ? '执行中'
      : settings.paused ? '已暂停'
        : dirty ? '未保存'
          : savedSettings.enabled ? '已启用' : '已关闭'
  const statusTone = settings.paused || dirty
    ? 'warning'
    : settings.running ? 'brand'
      : savedSettings?.enabled ? 'success' : 'neutral'

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
        const next = await saveAutoEarn(sourceId, settings, token)
        setSettings(next)
        setSavedSettings(next)
        setNotice('设置已保存')
      } else if (action === 'resume') {
        const next = await resumeAutoEarn(sourceId, token)
        setSettings(next)
        setSavedSettings(next)
        setNotice('暂停已解除')
      } else {
        const response = await runAutoEarn(sourceId, token)
        setNotice(response.result)
        await refresh()
      }
      setToken('')
      setTokenVisible(false)
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason)
      if (action !== 'save') await refresh()
      setError(message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <AppShell active="auto-earn" title="自动理财" subtitle="Binance STANDARD" icon={PiggyBank}>
      <PageIntro eyebrow="BFUSD" title="自动理财" />
      {error && <Alert tone="error" className="mb-5">{error}</Alert>}
      {notice && <Alert tone="success" className="mb-5">{notice}</Alert>}
      <div className="border-y border-border bg-surface px-4 py-4 sm:px-6">
        <div className="flex flex-col gap-4 sm:flex-row sm:items-end sm:justify-between">
          <Label className="w-full max-w-sm">账户
            <Select value={sourceId} onChange={(event) => setSourceId(event.target.value)} disabled={loading || busy}>
              {accounts.map((account) => <option key={account.source_id} value={account.source_id}>{account.account}</option>)}
            </Select>
          </Label>
          <div className="flex items-center gap-2 self-start sm:mb-1">
            <Badge tone={statusTone}>
              {settings.running && <LoaderCircle size={12} className="mr-1 animate-spin-slow" />}
              {statusLabel}
            </Badge>
            <Button type="button" size="sm" variant="ghost" className="h-8 w-8 p-0"
              onClick={() => void refresh()} title="刷新状态" aria-label="刷新状态" disabled={!sourceId || settingsLoading}>
              <RefreshCw size={16} />
            </Button>
          </div>
        </div>
      </div>
      {!loading && !sourceId && <Alert tone="warning" className="mt-6">没有可用的 Binance 账户</Alert>}
      {sourceId && settingsLoading && !savedSettings ? (
        <div className="flex h-48 items-center justify-center gap-2 text-sm text-muted">
          <LoaderCircle size={17} className="animate-spin-slow" /> 正在读取账户设置
        </div>
      ) : sourceId && !savedSettings ? (
        <Alert tone="warning" className="mt-6">账户设置暂不可用</Alert>
      ) : sourceId && (
        <div className="grid gap-8 py-7 lg:grid-cols-[minmax(0,1fr)_minmax(250px,320px)] lg:gap-10">
          <section className="min-w-0" aria-labelledby="auto-earn-settings">
            <div className="flex flex-wrap items-center justify-between gap-4 border-b border-border pb-5">
              <div className="flex items-center gap-2.5">
                <Settings2 size={17} className="text-brand" />
                <h3 id="auto-earn-settings" className="text-sm font-semibold text-ink">运行配置</h3>
              </div>
              <label className="inline-flex cursor-pointer items-center gap-3 text-sm font-medium text-ink">
                自动申购与收益划转
                <input type="checkbox" className="peer sr-only" checked={settings.enabled}
                  disabled={!configurable || busy || settingsLoading}
                  onChange={(event) => setSettings({ ...settings, enabled: event.target.checked })} />
                <span aria-hidden="true" className="relative h-6 w-11 shrink-0 rounded-full bg-border transition-colors after:absolute after:left-1 after:top-1 after:h-4 after:w-4 after:rounded-full after:bg-white after:shadow-sm after:transition-transform peer-checked:bg-brand peer-checked:after:translate-x-5 peer-focus-visible:ring-2 peer-focus-visible:ring-brand-ring peer-disabled:opacity-50" />
              </label>
            </div>
            <div className="grid gap-5 py-6 sm:grid-cols-3">
              <Label>执行间隔
                <div className="relative">
                  <Input type="number" min="1" max="1440" step="1" className="pr-14 tabular-nums [appearance:textfield] [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none" value={settings.interval_secs / 60}
                    disabled={!configurable || busy || settingsLoading}
                    onChange={(event) => setSettings({ ...settings, interval_secs: Number(event.target.value) * 60 })} />
                  <span className="pointer-events-none absolute inset-y-0 right-3 flex items-center text-xs text-muted">分钟</span>
                </div>
              </Label>
              <Label>每轮上限
                <div className="relative">
                  <Input type="number" min="1" max="1000000" step="0.01" className="pr-16 tabular-nums [appearance:textfield] [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none" value={settings.round_cap_usdt}
                    disabled={!configurable || busy || settingsLoading}
                    onChange={(event) => setSettings({ ...settings, round_cap_usdt: Number(event.target.value) })} />
                  <span className="pointer-events-none absolute inset-y-0 right-3 flex items-center text-xs text-muted">USDT</span>
                </div>
              </Label>
              <Label>触发下限
                <div className="relative">
                  <Input type="number" min="0" max="1000000" step="0.01" className="pr-16 tabular-nums [appearance:textfield] [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none" value={settings.trigger_usdt}
                    disabled={!configurable || busy || settingsLoading}
                    onChange={(event) => setSettings({ ...settings, trigger_usdt: Number(event.target.value) })} />
                  <span className="pointer-events-none absolute inset-y-0 right-3 flex items-center text-xs text-muted">USDT</span>
                </div>
              </Label>
            </div>
            {configurable && <div className="border-t border-border pt-6">
              <h3 className="mb-4 text-sm font-semibold text-ink">执行操作</h3>
              <div className="max-w-sm">
                <Label htmlFor="auto-earn-token" className="mb-1.5">操作 token</Label>
                <div className="relative">
                  <Input id="auto-earn-token" type={tokenVisible ? 'text' : 'password'} autoComplete="off" className="pr-11" value={token}
                    disabled={busy} onChange={(event) => setToken(event.target.value)} />
                  <button type="button" className="absolute inset-y-0 right-0 grid w-10 place-items-center text-muted hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-ring"
                    title={tokenVisible ? '隐藏 token' : '显示 token'} aria-label={tokenVisible ? '隐藏 token' : '显示 token'}
                    disabled={busy} onClick={() => setTokenVisible(!tokenVisible)}>
                    {tokenVisible ? <EyeOff size={16} /> : <Eye size={16} />}
                  </button>
                </div>
              </div>
              <div className="mt-5 flex flex-wrap gap-2">
                <Button variant="primary" disabled={busy || settingsLoading || !token || !dirty} onClick={() => void act('save')}>
                  <Check size={16} /> 保存设置
                </Button>
                <Button disabled={busy || settingsLoading || !token || dirty || !settings.enabled || settings.paused} onClick={() => void act('run')}>
                  <CirclePlay size={16} /> 立即执行
                </Button>
                {settings.paused && <Button variant="secondary" disabled={busy || !token || dirty} onClick={() => void act('resume')}>
                  <RotateCcw size={16} /> 解除暂停
                </Button>}
              </div>
            </div>}
          </section>
          <aside className="min-w-0 border-t border-border pt-6 lg:border-l lg:border-t-0 lg:pl-8 lg:pt-0" aria-labelledby="auto-earn-activity">
            <div className="flex items-center gap-2.5 border-b border-border pb-5">
              <Clock3 size={17} className="text-brand" />
              <h3 id="auto-earn-activity" className="text-sm font-semibold text-ink">最近执行</h3>
            </div>
            {settings.paused && <Alert tone="warning" className="mt-5">上次执行可能只完成了部分步骤，或检测到申购费用。核对最近结果及账户余额后再解除暂停。</Alert>}
            <div className="py-5 text-sm leading-6 text-muted">
              {settings.last_result ? (
                <div className="space-y-3 border-l-2 border-brand-ring pl-3">
                  {settings.last_result.split('; ').map((part, index) => (
                    <p key={index} className="break-words text-ink">{part}</p>
                  ))}
                </div>
              ) : <p>暂无执行记录</p>}
            </div>
          </aside>
        </div>
      )}
    </AppShell>
  )
}
