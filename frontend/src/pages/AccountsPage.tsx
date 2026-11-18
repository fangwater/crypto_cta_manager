import { accountExecutionGroup } from '../lib/executionGroups'
import { accountFeeSummary, fundingMoney } from '../format'
import { FundingStatusBadge } from '../components/FundingStatusBadge'
import { AccountAliasEditor } from '../components/AccountAliasEditor'
import {
  Activity,
  ArrowRight,
  BookOpen,
  Clock3,
  ExternalLink,
  LayoutDashboard,
  Layers3,
  RefreshCw,
  Scale,
  Settings,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { ApiError, getDashboard, getHealth } from '../api'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent } from '../components/ui/Card'
import { executionMarketLabel, formatUniMmr, integer, isUnifiedVenue, money, signedClass, timestampUs } from '../format'
import { AccountActionLink, AccountCardLayout, AccountMetric } from '../components/AccountCardLayout'
import { routes } from '../lib/routes'
import { accountSignalConfigPath } from '../lib/signalConfigLinks'
import { accountVenueLabel } from '../lib/workspace'
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
    target_proxy_enabled: false,
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

export function AccountsPage() {
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [health, setHealth] = useState<HealthResponse | null>(null)
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [navHistoryLoading, setNavHistoryLoading] = useState(false)

  const refresh = useCallback(async (signal?: AbortSignal, manual = false) => {
    if (manual) setRefreshing(true)
    try {
      const [nextDashboard, nextHealth] = await Promise.allSettled([
        getDashboard(signal),
        getHealth(signal),
      ])
      if (signal?.aborted) return
      if (nextHealth.status === 'fulfilled') setHealth(nextHealth.value)
      if (nextDashboard.status === 'rejected') throw nextDashboard.reason
      setDashboard(nextDashboard.value)
      setNavHistoryLoading(false)
      if (nextHealth.status === 'rejected') throw nextHealth.reason
      setError(null)
    } catch (reason: unknown) {
      if (signal?.aborted || (reason instanceof DOMException && reason.name === 'AbortError')) return
      if (reason instanceof ApiError && reason.historyLoading) {
        setNavHistoryLoading(true)
        setError(null)
        return
      }
      setNavHistoryLoading(false)
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      if (!signal?.aborted) {
        setLoading(false)
        if (manual) setRefreshing(false)
      }
    }
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    void refresh(controller.signal)
    const timer = window.setInterval(() => void refresh(controller.signal), UI_POLL_MS)
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

  function updateAlias(update: { source_id: string; account: string; display_alias: string | null }) {
    setDashboard(current => current ? {
      ...current,
      accounts: current.accounts?.map(account => account.source_id === update.source_id ? { ...account, ...update } : account),
      report: { ...current.report, sources: current.report.sources.map(source => source.source_id === update.source_id ? { ...source, account: update.account } : source) },
    } : current)
  }

  return (
    <AppShell
      active="accounts"
      title="CTA Manager"
      subtitle="账户管理"
      icon={LayoutDashboard}
      actions={
        <div className="flex items-center gap-2">
          <Badge tone={health?.status === 'ok' && !navHistoryLoading ? 'success' : error || health?.status === 'degraded' ? 'warning' : 'neutral'} className="hidden sm:inline-flex">
            {navHistoryLoading || health?.status === 'loading' ? '净值加载中' : health ? health.status === 'ok' ? '服务在线' : '数据延迟' : error ? '连接失败' : '检查中'}
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
        eyebrow="账户管理"
        title="账户入口"
        description="从这里进入各账户配置概览、净值曲线与 Exec Viz。"
        actions={
          <div className="flex flex-wrap items-center gap-3 text-xs text-muted">
            <Clock3 size={14} />{timestampUs(dashboard?.generated_at_us ?? null)}
          </div>
        }
      />

      {error && <Alert tone="error" className="mb-4">数据请求失败：{error}</Alert>}
      {navHistoryLoading && <div className="chart-notice mb-4" role="status">净值历史正在后台加载，完成后自动显示。</div>}
      {health?.last_refresh_error && (
        <Alert tone="warning" className="mb-4">
          最近一次服务端重算失败，当前显示上一份有效数据
        </Alert>
      )}

      <div className="mb-8 grid gap-3 sm:grid-cols-2">
        <StatTile label="运行账户" value={dashboard ? integer(activeAccounts) : '--'} />
        <StatTile label="已配置账户" value={dashboard ? integer(accounts.length) : '--'} />
      </div>

      <a href={routes.virtualAccounts} className="mb-6 flex items-center justify-between gap-4 rounded-2xl border border-border bg-surface p-5 transition hover:border-brand-ring">
        <div className="flex min-w-0 items-center gap-3"><Layers3 size={20} className="shrink-0 text-brand" /><div><p className="font-semibold text-ink">虚拟账户管理</p><p className="mt-1 text-sm text-muted">维护策略组合，查看跟随倍率与同步状态。</p></div></div>
        <ArrowRight size={18} className="shrink-0 text-muted" />
      </a>

      <a
        href="/manager/docs/"
        className="mb-8 flex flex-col gap-4 rounded-2xl border border-brand-ring/40 bg-gradient-to-br from-brand-soft to-surface p-5 shadow-[var(--shadow-card)] transition hover:border-brand/40 sm:flex-row sm:items-center sm:justify-between"
      >
        <div className="flex items-start gap-3">
          <div className="grid h-11 w-11 place-items-center rounded-xl bg-surface text-brand shadow-sm">
            <BookOpen size={20} />
          </div>
          <div>
            <p className="text-[11px] font-semibold uppercase tracking-[0.16em] text-brand">操作指南</p>
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
          <p className="text-[11px] font-semibold uppercase tracking-[0.16em] text-brand">账户列表</p>
          <h3 className="mt-1 text-xl font-semibold text-ink">账户入口</h3>
        </div>
        <span className="text-xs text-muted">重算 {dashboard?.generation_duration_ms ?? 0} ms</span>
      </div>

      {(loading || navHistoryLoading) && !dashboard ? (
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
              onAliasSaved={updateAlias}
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
  onAliasSaved,
}: {
  account: DashboardAccount
  report?: SourceNavReport
  health: HealthResponse | null
  onAliasSaved: (update: { source_id: string; account: string; display_alias: string | null }) => void
}) {
  const ready = account.enabled && report !== undefined
  const status = !account.enabled
    ? '待接入'
    : !report
      ? '等待数据'
      : !health
        ? '检查中'
        : health.status === 'ok'
        ? '数据就绪'
        : '数据延迟'
  const funding = account.cumulative_funding
  const combinedNav = report && funding?.income_quote != null
    ? report.nav_change_after_fee_quote + funding.income_quote : null
  const displayedNav = funding ? combinedNav ?? (account.execution_sources?.length ? report?.nav_change_after_fee_quote : null) : report?.nav_change_after_fee_quote
  const executionGroup = accountExecutionGroup(account)
  const executionUrl = !ready ? null : executionGroup ? routes.execViz(executionGroup.id) : account.gateway_prefix ? `${account.gateway_prefix}/` : null
  const signalConfigPath = account.enabled && account.access_level === 'configure'
    ? accountSignalConfigPath(account.source_id)
    : undefined
  const openPositions = account.open_position_symbol_count ??
    report?.symbols.filter(
      (symbol) =>
        Math.abs(symbol.long_position_value_quote) > 1e-9 ||
        Math.abs(symbol.short_position_value_quote) > 1e-9,
    ).length ?? 0

  return (
    <AccountCardLayout dimmed={!ready} mark={venueMark(account.venue)}
      title={<AccountAliasEditor account={account} onSaved={onAliasSaved} />}
      sourceId={account.execution_sources?.length ? undefined : account.source_id}
      badges={<>
        <Badge tone={account.venue === 'binance-coin-futures' ? 'warning' : 'brand'}>{executionMarketLabel(account.venue)}</Badge>
        {account.venue === 'binance-futures' && <FundingStatusBadge value={account.funding} />}
        {(account.unified_account || isUnifiedVenue(account.venue)) && (
          <Badge tone={account.unified_account ? 'brand' : 'neutral'}>统一账户</Badge>
        )}
        <Badge tone={ready && health ? health.status === 'ok' ? 'success' : 'warning' : 'neutral'}>{status}</Badge>
      </>}>
        {!!account.execution_sources?.length && <p className="text-sm text-muted">{account.execution_sources.map(leg => accountVenueLabel(leg.venue)).join(' / ')}</p>}
        <div className="grid grid-cols-2 gap-3 text-sm">
          <AccountMetric
            label={funding?.income_quote != null ? '累计费后净值（含 Funding）' : '累计费后净值'}
            value={displayedNav != null ? `${money(displayedNav)} 报价币` : '--'}
            tone={displayedNav != null ? signedClass(displayedNav) : ''}
          />
          <AccountMetric
            label="实时权益"
            value={
              account.live_equity_usdt != null
                ? `${money(account.live_equity_usdt)} USDT`
                : '--'
            }
          />
          {(account.unified_account || isUnifiedVenue(account.venue)) && (
            <AccountMetric
              label="UniMMR"
              value={
                account.uni_mmr_status === 'stale'
                  ? `${formatUniMmr(account.uni_mmr)} 延迟`
                  : formatUniMmr(account.uni_mmr)
              }
              tone={uniMmrTone(account.uni_mmr, account.uni_mmr_status)}
            />
          )}
          {funding && <>
            <AccountMetric label="订单费后净值" value={report ? `${money(report.nav_change_after_fee_quote)} 报价币` : '--'} />
            <AccountMetric label={`Funding（${funding.kind === 'actual' ? '实际' : '估算'}）`}
              value={funding.income_quote != null ? `${fundingMoney(funding.income_quote)} 报价币` : '--'}
              title={funding.income_quote != null ? `统计截至 ${timestampUs(funding.end_ts_us)}${funding.refreshing ? '，更新中' : ''}` : undefined}
              tone={funding.income_quote != null ? signedClass(funding.income_quote) : ''} />
          </>}
          <AccountMetric label="当前持仓" value={report ? `${openPositions} 个合约` : '--'} />
          <AccountMetric
            label="Maker / Taker 费率"
            value={report ? accountFeeSummary(report) : '--'}
          />
          <AccountMetric label="最近成交" value={timestampUs(report?.last_fill_ts_us ?? null)} />
        </div>
        {funding && !funding.complete && <p className="text-xs text-muted" title={funding.unavailable_reason ?? undefined}>
          {funding.loading ? 'Funding 补齐中' : funding.income_quote != null ? 'Funding 部分覆盖：仅计入已知收支' : 'Funding 暂不可用'}
        </p>}
        <div className="flex flex-wrap gap-2 border-t border-border-soft pt-4">
          {ready ? (
            <>
              <AccountActionLink
                href={routes.account(account.source_id)}
                primary
                icon={<Settings size={15} />}
                label="查看配置"
              />
              <AccountActionLink
                href={routes.nav(account.source_id)}
                icon={<Activity size={15} />}
                label="净值"
              />
              <AccountActionLink
                href={routes.executionCost(account.source_id)}
                icon={<Scale size={15} />}
                label="成本"
              />
            </>
          ) : (
            <>
              <ActionDisabled icon={<Settings size={15} />} label="查看配置" />
              <ActionDisabled icon={<Activity size={15} />} label="净值" />
              <ActionDisabled icon={<Scale size={15} />} label="成本" />
            </>
          )}
          {executionUrl ? (
            <AccountActionLink
              href={executionUrl}
              icon={<ExternalLink size={15} />}
              label="执行监控"
            />
          ) : (
            <ActionDisabled icon={<ExternalLink size={15} />} label="执行监控" />
          )}
          {signalConfigPath && (
            <AccountActionLink
              href={signalConfigPath}
              icon={<Settings size={15} />}
              label="信号配置"
            />
          )}
        </div>
    </AccountCardLayout>
  )
}

function uniMmrTone(value: number | null | undefined, status?: string | null) {
  if (status === 'stale' || value == null || !Number.isFinite(value)) return 'text-muted'
  if (value <= 1.05) return 'text-rose-700'
  if (value <= 1.5) return 'text-warning'
  return ''
}

function ActionDisabled({ icon, label }: { icon: React.ReactNode; label: string }) {
  return (
    <span className="inline-flex items-center gap-2 rounded-lg border border-border-soft px-3 py-2 text-sm text-subtle">
      {icon}
      {label}
    </span>
  )
}
