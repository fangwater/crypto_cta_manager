import {
  Activity,
  ArrowRight,
  BookOpen,
  Clock3,
  ExternalLink,
  LayoutDashboard,
  RefreshCw,
  Settings,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { getDashboard, getHealth } from '../api'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/Card'
import { feeBps, integer, money, signedClass, timestampUs } from '../format'
import { cn } from '../lib/cn'
import type {
  DashboardAccount,
  DashboardSnapshot,
  HealthResponse,
  SourceNavReport,
} from '../types'

const UI_POLL_MS = 15_000

function fallbackAccounts(dashboard: DashboardSnapshot): DashboardAccount[] {
  return dashboard.report.sources.map((source) => ({
    source_id: source.source_id,
    account: source.account,
    venue: source.configured_venue,
    enabled: true,
    gateway_prefix: null,
    configurable: false,
  }))
}

function venueMark(venue: string) {
  const normalized = venue.toLowerCase()
  if (normalized.includes('binance')) return 'B'
  if (normalized.includes('bybit')) return 'Y'
  if (normalized.includes('gate')) return 'G'
  if (normalized.includes('okx') || normalized.includes('okex')) return 'O'
  if (normalized.includes('bitget')) return 'BG'
  return venue.slice(0, 2).toUpperCase()
}

export function WorkspacePage() {
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [health, setHealth] = useState<HealthResponse | null>(null)
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(async (signal?: AbortSignal, manual = false) => {
    if (manual) setRefreshing(true)
    try {
      const [nextDashboard, nextHealth] = await Promise.all([
        getDashboard(signal),
        getHealth(signal),
      ])
      setDashboard(nextDashboard)
      setHealth(nextHealth)
      setError(null)
    } catch (reason: unknown) {
      if (reason instanceof DOMException && reason.name === 'AbortError') return
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setLoading(false)
      if (manual) setRefreshing(false)
    }
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    void refresh(controller.signal)
    const timer = window.setInterval(() => void refresh(), UI_POLL_MS)
    return () => {
      controller.abort()
      window.clearInterval(timer)
    }
  }, [refresh])

  const accounts = useMemo(
    () =>
      dashboard
        ? dashboard.accounts?.length
          ? dashboard.accounts
          : fallbackAccounts(dashboard)
        : [],
    [dashboard],
  )
  const reportsBySource = useMemo(
    () => new Map((dashboard?.report.sources ?? []).map((source) => [source.source_id, source])),
    [dashboard],
  )
  const activeAccounts = accounts.filter(
    (account) => account.enabled && reportsBySource.has(account.source_id),
  ).length
  const aggregate = dashboard?.report.aggregate
  const openSymbolCount =
    aggregate?.symbols.filter(
      (symbol) =>
        Math.abs(symbol.long_position_value_quote) > 1e-9 ||
        Math.abs(symbol.short_position_value_quote) > 1e-9,
    ).length ?? 0
  const grossExposure =
    aggregate?.symbols.reduce(
      (total, symbol) =>
        total +
        Math.abs(symbol.long_position_value_quote) +
        Math.abs(symbol.short_position_value_quote),
      0,
    ) ?? 0

  return (
    <AppShell
      active="workspace"
      title="CTA Manager"
      subtitle="综合交易工作台"
      icon={LayoutDashboard}
      actions={
        <div className="flex items-center gap-2">
          <Badge tone={health?.status === 'ok' ? 'success' : 'warning'} className="hidden sm:inline-flex">
            {health?.status === 'ok' ? '服务在线' : '数据延迟'}
          </Badge>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            disabled={refreshing}
            onClick={() => void refresh(undefined, true)}
            aria-label="刷新数据"
          >
            <RefreshCw size={15} className={refreshing ? 'animate-spin-slow' : ''} />
          </Button>
        </div>
      }
    >
      <PageIntro
        eyebrow="Workspace"
        title="盘子总览"
        description="从这里进入各账户净值、Exec Viz 和策略组合配置。"
        actions={
          <div className="flex items-center gap-2 text-xs text-muted">
            <Clock3 size={14} />
            {timestampUs(dashboard?.generated_at_us ?? null)}
          </div>
        }
      />

      {error && <Alert tone="error" className="mb-4">数据请求失败：{error}</Alert>}
      {health?.last_refresh_error && (
        <Alert tone="warning" className="mb-4">
          最近一次服务端重算失败，当前显示上一份有效数据
        </Alert>
      )}

      <div className="mb-8 grid gap-3 sm:grid-cols-2 xl:grid-cols-5">
        <StatTile label="运行账户" value={`${activeAccounts} / ${accounts.length}`} />
        <StatTile
          label="累计费后净值"
          value={aggregate ? money(aggregate.nav_change_after_fee_quote) : '--'}
          hint={aggregate ? 'USDT' : undefined}
        />
        <StatTile
          label="当前浮动盈亏"
          value={aggregate ? money(aggregate.floating_pnl_quote) : '--'}
          hint={aggregate ? 'USDT' : undefined}
        />
        <StatTile
          label="当前总敞口"
          value={dashboard ? money(grossExposure) : '--'}
          hint={dashboard ? 'USDT' : undefined}
        />
        <StatTile
          label="持仓 / 成交"
          value={dashboard ? `${openSymbolCount} / ${integer(aggregate?.fill_count ?? 0)}` : '--'}
        />
      </div>

      <a
        href="/manager/docs/"
        className="mb-8 flex flex-col gap-4 rounded-2xl border border-brand-ring/40 bg-gradient-to-br from-brand-soft to-surface p-5 shadow-[var(--shadow-card)] transition hover:border-brand/40 sm:flex-row sm:items-center sm:justify-between"
      >
        <div className="flex items-start gap-3">
          <div className="grid h-11 w-11 place-items-center rounded-xl bg-surface text-brand shadow-sm">
            <BookOpen size={20} />
          </div>
          <div>
            <p className="text-[11px] font-semibold uppercase tracking-[0.16em] text-brand">Documentation</p>
            <h3 className="mt-1 text-lg font-semibold text-ink">操作文档</h3>
            <p className="mt-1 text-sm text-muted">策略组合、账户入口、推送仓位与 Config API</p>
          </div>
        </div>
        <span className="inline-flex items-center gap-2 text-sm font-medium text-brand">
          打开文档 <ArrowRight size={16} />
        </span>
      </a>

      <div className="mb-4 flex items-end justify-between gap-3">
        <div>
          <p className="text-[11px] font-semibold uppercase tracking-[0.16em] text-brand">Accounts</p>
          <h3 className="mt-1 text-xl font-semibold text-ink">账户入口</h3>
        </div>
        <span className="text-xs text-muted">重算 {dashboard?.generation_duration_ms ?? 0} ms</span>
      </div>

      {loading && !dashboard ? (
        <div className="grid gap-4 md:grid-cols-2">
          <div className="h-56 animate-pulse rounded-2xl bg-border-soft" />
          <div className="h-56 animate-pulse rounded-2xl bg-border-soft" />
        </div>
      ) : (
        <div className="grid gap-4 md:grid-cols-2">
          {accounts.map((account) => (
            <AccountCard
              key={account.source_id}
              account={account}
              report={reportsBySource.get(account.source_id)}
              health={health}
            />
          ))}
          {accounts.length === 0 && (
            <Card>
              <CardContent className="py-12 text-center text-sm text-muted">暂无已配置账户</CardContent>
            </Card>
          )}
        </div>
      )}
    </AppShell>
  )
}

function AccountCard({
  account,
  report,
  health,
}: {
  account: DashboardAccount
  report?: SourceNavReport
  health: HealthResponse | null
}) {
  const ready = account.enabled && report !== undefined
  const status = !account.enabled
    ? '待接入'
    : !report
      ? '等待数据'
      : health?.status === 'ok'
        ? '数据就绪'
        : '数据延迟'
  const gatewayReady = ready && account.gateway_prefix !== null
  const openPositions =
    report?.symbols.filter(
      (symbol) =>
        Math.abs(symbol.long_position_value_quote) > 1e-9 ||
        Math.abs(symbol.short_position_value_quote) > 1e-9,
    ).length ?? 0

  return (
    <Card className={cn(!ready && 'opacity-80')}>
      <CardHeader className="flex flex-row items-start justify-between gap-3">
        <div className="flex items-start gap-3">
          <div className="grid h-10 w-10 place-items-center rounded-xl bg-canvas text-sm font-bold text-brand">
            {venueMark(account.venue)}
          </div>
          <div>
            <CardTitle className="text-base">{account.account}</CardTitle>
            <CardDescription className="font-mono text-[11px]">{account.source_id}</CardDescription>
          </div>
        </div>
        <Badge tone={ready ? 'success' : 'neutral'}>{status}</Badge>
      </CardHeader>
      <CardContent className="space-y-4 pt-0">
        <div className="grid grid-cols-2 gap-3 text-sm">
          <Metric
            label="累计费后净值"
            value={report ? `${money(report.nav_change_after_fee_quote)} USDT` : '--'}
            tone={report ? signedClass(report.nav_change_after_fee_quote) : ''}
          />
          <Metric label="当前持仓" value={report ? `${openPositions} symbols` : '--'} />
          <Metric label="估算费率" value={report ? feeBps(report.estimated_fee_rate) : '--'} />
          <Metric label="最近成交" value={timestampUs(report?.last_fill_ts_us ?? null)} />
        </div>
        <div className="flex flex-wrap gap-2 border-t border-border-soft pt-4">
          {ready ? (
            <ActionLink
              href={`/manager/?source=${encodeURIComponent(account.source_id)}`}
              primary
              icon={<Activity size={15} />}
              label="净值"
            />
          ) : (
            <ActionDisabled icon={<Activity size={15} />} label="净值" />
          )}
          {gatewayReady ? (
            <ActionLink
              href={`${account.gateway_prefix}/`}
              icon={<ExternalLink size={15} />}
              label="Exec Viz"
            />
          ) : (
            <ActionDisabled icon={<ExternalLink size={15} />} label="Exec Viz" />
          )}
          {ready && account.configurable ? (
            <ActionLink
              href={`/manager/config/?source=${encodeURIComponent(account.source_id)}`}
              icon={<Settings size={15} />}
              label="配置"
            />
          ) : (
            <ActionDisabled icon={<Settings size={15} />} label="配置" />
          )}
        </div>
      </CardContent>
    </Card>
  )
}

function Metric({ label, value, tone = '' }: { label: string; value: string; tone?: string }) {
  return (
    <div>
      <p className="text-xs text-muted">{label}</p>
      <p className={cn('mt-1 font-medium tabular-nums text-ink', tone)}>{value}</p>
    </div>
  )
}

function ActionLink({
  href,
  icon,
  label,
  primary = false,
}: {
  href: string
  icon: React.ReactNode
  label: string
  primary?: boolean
}) {
  return (
    <a
      href={href}
      className={cn(
        'inline-flex items-center gap-2 rounded-lg px-3 py-2 text-sm font-medium transition-colors',
        primary
          ? 'bg-brand text-white hover:bg-brand-hover'
          : 'border border-border bg-surface text-ink hover:bg-canvas',
      )}
    >
      {icon}
      {label}
      {primary && <ArrowRight size={14} />}
    </a>
  )
}

function ActionDisabled({ icon, label }: { icon: React.ReactNode; label: string }) {
  return (
    <span className="inline-flex items-center gap-2 rounded-lg border border-border-soft px-3 py-2 text-sm text-subtle">
      {icon}
      {label}
    </span>
  )
}
