import {
  Activity,
  ArrowRight,
  CheckCircle2,
  Clock3,
  Database,
  ExternalLink,
  LayoutDashboard,
  RefreshCw,
  Server,
  Settings,
  WalletCards,
  BookOpen,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { getDashboard, getHealth } from '../api'
import { AppNav } from '../components/AppNav'
import { feeBps, integer, money, signedClass, timestampUs } from '../format'
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
    () =>
      new Map(
        (dashboard?.report.sources ?? []).map((source) => [
          source.source_id,
          source,
        ]),
      ),
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
    <div className="app-frame">
      <header className="app-header">
        <div className="app-header__inner">
          <div className="brand">
            <span className="brand__mark" aria-hidden="true">
              <LayoutDashboard size={19} strokeWidth={2.1} />
            </span>
            <div>
              <h1>CTA Manager</h1>
              <p>综合交易工作台</p>
            </div>
          </div>
          <div className="header-actions">
            <AppNav active="workspace" />
            <div className="header-state">
              <span
                className={`status-dot ${health?.status === 'ok' ? 'status-dot--ready' : 'status-dot--warning'}`}
              />
              <div>
                <span>{health?.status === 'ok' ? '服务在线' : '数据延迟'}</span>
                <time>{timestampUs(dashboard?.generated_at_us ?? null)}</time>
              </div>
              <button
                type="button"
                className="icon-button"
                title="刷新数据"
                aria-label="刷新数据"
                disabled={refreshing}
                onClick={() => void refresh(undefined, true)}
              >
                <RefreshCw size={17} className={refreshing ? 'is-spinning' : ''} />
              </button>
            </div>
          </div>
        </div>
      </header>

      <main className="page-shell workspace-shell">
        {error && <div className="error-banner">数据请求失败：{error}</div>}
        {health?.last_refresh_error && (
          <div className="warning-banner">
            最近一次服务端重算失败，当前显示上一份有效数据
          </div>
        )}

        <section className="workspace-overview" aria-labelledby="workspace-title">
          <div className="section-heading">
            <div>
              <p className="eyebrow">CTA WORKSPACE</p>
              <h2 id="workspace-title">盘子总览</h2>
            </div>
            <div className="workspace-heading-actions">
              <a className="gitbook-jump" href="/manager/docs/">
                <BookOpen size={14} />
                文档
              </a>
              <span className="workspace-updated">
                <Clock3 size={14} />
                {timestampUs(dashboard?.generated_at_us ?? null)}
              </span>
            </div>
          </div>

          <div className="summary-strip workspace-summary">
            <WorkspaceStat
              icon={<Server size={18} />}
              label="运行账户"
              value={`${activeAccounts} / ${accounts.length}`}
            />
            <WorkspaceStat
              icon={<WalletCards size={18} />}
              label="累计费后净值"
              value={aggregate ? money(aggregate.nav_change_after_fee_quote) : '--'}
              unit={aggregate ? 'USDT' : undefined}
              tone={aggregate ? signedClass(aggregate.nav_change_after_fee_quote) : ''}
            />
            <WorkspaceStat
              icon={<Activity size={18} />}
              label="当前浮动盈亏"
              value={aggregate ? money(aggregate.floating_pnl_quote) : '--'}
              unit={aggregate ? 'USDT' : undefined}
              tone={aggregate ? signedClass(aggregate.floating_pnl_quote) : ''}
            />
            <WorkspaceStat
              icon={<Database size={18} />}
              label="当前总敞口"
              value={dashboard ? money(grossExposure) : '--'}
              unit={dashboard ? 'USDT' : undefined}
            />
            <WorkspaceStat
              icon={<CheckCircle2 size={18} />}
              label="持仓 / 成交"
              value={dashboard ? `${openSymbolCount} / ${integer(aggregate?.fill_count ?? 0)}` : '--'}
            />
          </div>
        </section>

        <section className="account-section" aria-labelledby="accounts-title">
          <div className="panel-heading">
            <div>
              <p className="eyebrow">ACCOUNTS & SERVICES</p>
              <h2 id="accounts-title">账户入口</h2>
            </div>
            <span className="generation-time">
              重算 {dashboard?.generation_duration_ms ?? 0} ms
            </span>
          </div>

          <a className="docs-entry-card" href="/manager/docs/">
            <span className="docs-entry-card__mark" aria-hidden="true">
              <BookOpen size={20} strokeWidth={2.1} />
            </span>
            <div>
              <p className="eyebrow">DOCUMENTATION</p>
              <h3>操作文档</h3>
              <p>查看策略组合、账户入口、推送仓位，以及 Config API</p>
            </div>
            <span className="docs-entry-card__go">
              打开文档
              <ArrowRight size={16} />
            </span>
          </a>

          {loading && !dashboard ? (
            <div className="account-grid account-grid--loading" aria-label="正在加载账户">
              <div />
              <div />
            </div>
          ) : (
            <div className="account-grid">
              {accounts.map((account) => (
                <AccountCard
                  key={account.source_id}
                  account={account}
                  report={reportsBySource.get(account.source_id)}
                  health={health}
                />
              ))}
              {accounts.length === 0 && (
                <div className="account-empty">暂无已配置账户</div>
              )}
            </div>
          )}
        </section>
      </main>
    </div>
  )
}

function WorkspaceStat({
  icon,
  label,
  value,
  unit,
  tone = '',
}: {
  icon: React.ReactNode
  label: string
  value: string
  unit?: string
  tone?: string
}) {
  return (
    <div className="summary-item">
      {icon}
      <div>
        <span>{label}</span>
        <strong className={tone}>
          {value} {unit && <small>{unit}</small>}
        </strong>
      </div>
    </div>
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
    <article className={`account-card ${ready ? '' : 'account-card--pending'}`}>
      <header className="account-card__head">
        <div className="account-identity">
          <span className="venue-mark">{venueMark(account.venue)}</span>
          <div>
            <h3>{account.account}</h3>
            <code>{account.source_id}</code>
          </div>
        </div>
        <span className={`account-state ${ready ? 'account-state--ready' : ''}`}>
          <span className="status-dot" />
          {status}
        </span>
      </header>

      <div className="account-metrics">
        <AccountMetric
          label="累计费后净值"
          value={report ? `${money(report.nav_change_after_fee_quote)} USDT` : '--'}
          tone={report ? signedClass(report.nav_change_after_fee_quote) : ''}
        />
        <AccountMetric label="当前持仓" value={report ? `${openPositions} symbols` : '--'} />
        <AccountMetric
          label="估算费率"
          value={report ? feeBps(report.estimated_fee_rate) : '--'}
        />
        <AccountMetric label="最近成交" value={timestampUs(report?.last_fill_ts_us ?? null)} />
      </div>

      <footer className="account-card__actions">
        {ready ? (
          <a
            className="account-action account-action--primary"
            href={`/manager/?source=${encodeURIComponent(account.source_id)}`}
          >
            <Activity size={15} />
            净值
            <ArrowRight size={14} />
          </a>
        ) : (
          <span className="account-action is-disabled">
            <Activity size={15} />
            净值
          </span>
        )}
        {gatewayReady ? (
          <a className="account-action" href={`${account.gateway_prefix}/`}>
            <ExternalLink size={15} />
            Exec Viz
          </a>
        ) : (
          <span className="account-action is-disabled">
            <ExternalLink size={15} />
            Exec Viz
          </span>
        )}
        {ready && account.configurable ? (
          <a
            className="account-action"
            href={`/manager/config/?source=${encodeURIComponent(account.source_id)}`}
          >
            <Settings size={15} />
            配置
          </a>
        ) : (
          <span className="account-action is-disabled">
            <Settings size={15} />
            配置
          </span>
        )}
      </footer>
    </article>
  )
}

function AccountMetric({
  label,
  value,
  tone = '',
}: {
  label: string
  value: string
  tone?: string
}) {
  return (
    <div>
      <span>{label}</span>
      <strong className={tone}>{value}</strong>
    </div>
  )
}
