import {
  Activity,
  ArrowRight,
  ExternalLink,
  Layers3,
  LoaderCircle,
  PencilLine,
  RefreshCw,
  SlidersHorizontal,
  WalletCards,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { getAccountLive, getAccountStudio, getDashboard } from '../api'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { CapacityPanel } from '../components/CapacityPanel'
import { OrderParametersView } from '../components/OrderParametersView'
import { TargetPositionsView } from '../components/TargetPositionsView'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/Card'
import { useStrategyCatalog } from '../hooks/useStrategyCatalog'
import { feeBps, money, signedClass, timestampUs } from '../format'
import { percent } from '../lib/strategyDefaults'
import { readSourceId, routes } from '../lib/routes'
import { cn } from '../lib/cn'
import type {
  AccountCapacity,
  AccountStudio,
  DashboardAccount,
  DashboardSnapshot,
  SourceNavReport,
} from '../types'

function venueMark(venue: string) {
  const normalized = venue.toLowerCase()
  if (normalized.includes('binance')) return 'B'
  return venue.slice(0, 2).toUpperCase()
}

export function AccountOverviewPage() {
  const sourceId = readSourceId()
  const { positions, orders, loading: catalogLoading } = useStrategyCatalog()
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [studio, setStudio] = useState<AccountStudio | null>(null)
  const [capacity, setCapacity] = useState<AccountCapacity | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(async (signal?: AbortSignal) => {
    if (!sourceId) return
    const snapshot = await getDashboard(signal)
    const nextStudio = await getAccountStudio(sourceId, signal)
    setDashboard(snapshot)
    setStudio(nextStudio)
    setCapacity(nextStudio.capacity ?? null)
  }, [sourceId])

  useEffect(() => {
    if (!sourceId) return
    const timer = window.setInterval(() => {
      void getAccountLive(sourceId)
        .then(setCapacity)
        .catch(() => undefined)
    }, 2_000)
    return () => window.clearInterval(timer)
  }, [sourceId])

  useEffect(() => {
    if (!sourceId) {
      setLoading(false)
      return
    }
    const controller = new AbortController()
    refresh(controller.signal)
      .then(() => setError(null))
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => setLoading(false))
    return () => controller.abort()
  }, [refresh, sourceId])

  const account = useMemo(() => {
    const accounts = dashboard?.accounts ?? []
    return accounts.find((entry) => entry.source_id === sourceId)
  }, [dashboard, sourceId])

  const report = useMemo(
    () => dashboard?.report.sources.find((source) => source.source_id === sourceId),
    [dashboard, sourceId],
  )

  const positionByName = useMemo(
    () => new Map(positions.map((item) => [item.strategy_name, item])),
    [positions],
  )
  const orderByName = useMemo(
    () => new Map(orders.map((item) => [item.strategy_name, item])),
    [orders],
  )

  if (!sourceId) {
    return (
      <AppShell active="workspace" title="账户配置" subtitle="只读概览" icon={WalletCards}>
        <Alert tone="warning">缺少 source 参数。请从总览页进入某个账户。</Alert>
        <a href={routes.workspace} className="mt-4 inline-flex text-sm font-medium text-brand">
          返回总览
        </a>
      </AppShell>
    )
  }

  return (
    <AppShell
      active="workspace"
      title={account?.account ?? sourceId}
      subtitle="账户配置概览"
      icon={WalletCards}
      actions={
        <Button type="button" size="sm" variant="ghost" onClick={() => void refresh()}>
          <RefreshCw size={15} />
        </Button>
      }
    >
      {loading || catalogLoading ? (
        <div className="flex items-center justify-center gap-2 py-24 text-sm text-muted">
          <LoaderCircle size={18} className="animate-spin-slow" />
          正在加载账户配置
        </div>
      ) : (
        <>
          {error && <Alert tone="error" className="mb-4">{error}</Alert>}

          <PageIntro
            eyebrow="Account Overview"
            title={account?.account ?? sourceId}
            description="这里只展示当前组合配置与运行摘要。修改请进入右侧独立的策略配置分区。"
            actions={
              <div className="flex flex-wrap gap-2">
                <ActionButton href={routes.nav(sourceId)} primary icon={<Activity size={15} />}>
                  净值
                </ActionButton>
                {account?.gateway_prefix && (
                  <ActionButton href={`${account.gateway_prefix}/`} icon={<ExternalLink size={15} />}>
                    Exec Viz
                  </ActionButton>
                )}
                <ActionButton
                  href={routes.configBindings(sourceId)}
                  icon={<PencilLine size={15} />}
                >
                  编辑启用
                </ActionButton>
              </div>
            }
          />

          <div className="mb-8">
            <CapacityPanel capacity={capacity ?? studio?.capacity} />
          </div>

          <div className="mb-8 grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
            <StatTile label="杠杆率" value={studio ? String(studio.leverage) : '--'} />
            <StatTile
              label="参考权益合计"
              value={studio ? money(studio.bound_equity_usdt) : '--'}
              hint="USDT"
            />
            <StatTile
              label="累计费后净值"
              value={report ? money(report.nav_change_after_fee_quote) : '--'}
              hint="USDT"
            />
            <StatTile label="估算费率" value={report ? feeBps(report.estimated_fee_rate) : '--'} />
          </div>

          <section className="mb-8">
            <SectionHeading
              icon={Layers3}
              title="策略启用"
              description={`${studio?.bindings.length ?? 0} 条策略 · 每条策略绑定一个执行算法`}
            />
            {(studio?.bindings ?? []).length === 0 ? (
              <Card>
                <CardContent className="py-12 text-center text-sm text-muted">
                  尚未绑定策略组合。
                  <a href={routes.configBindings(sourceId)} className="ml-1 font-medium text-brand">
                    去配置
                  </a>
                </CardContent>
              </Card>
            ) : (
              <div className="grid gap-4 xl:grid-cols-2">
                {(studio?.bindings ?? []).map((binding) => {
                  const position = positionByName.get(binding.position_strategy_name)
                  const order = orderByName.get(binding.order_strategy_name)
                  return (
                    <Card
                      key={binding.binding_name}
                      className="overflow-hidden border-border-soft bg-gradient-to-br from-surface to-canvas/40"
                    >
                      <CardHeader className="border-b border-border-soft/80">
                        <div className="flex items-start justify-between gap-3">
                          <div>
                            <CardTitle className="text-base">{binding.binding_name}</CardTitle>
                            <CardDescription className="mt-1 font-mono text-[11px]">
                              Exec 策略名
                            </CardDescription>
                          </div>
                          <Badge tone="brand">{percent(binding.allocation_ratio)}</Badge>
                        </div>
                      </CardHeader>
                      <CardContent className="space-y-5 pt-5">
                        <div className="grid gap-3 sm:grid-cols-2">
                          <MiniStat label="仓位策略" value={binding.position_strategy_name} mono />
                          <MiniStat label="执行算法" value={binding.order_strategy_name} mono />
                          <MiniStat
                            label="参考权益"
                            value={`${money(binding.position_equity_usdt)} USDT`}
                          />
                          <MiniStat
                            label="比例"
                            value={percent(binding.allocation_ratio)}
                          />
                        </div>
                        {position && (
                          <div>
                            <p className="mb-2 text-xs font-medium uppercase tracking-[0.12em] text-subtle">
                              目标仓位
                            </p>
                            <TargetPositionsView targets={position.targets} compact />
                          </div>
                        )}
                        {order && (
                          <div>
                            <p className="mb-2 text-xs font-medium uppercase tracking-[0.12em] text-subtle">
                              下单参数
                            </p>
                            <OrderParametersView value={order.order_parameters} />
                          </div>
                        )}
                      </CardContent>
                    </Card>
                  )
                })}
              </div>
            )}
          </section>

          <section>
            <SectionHeading
              icon={SlidersHorizontal}
              title="配置入口"
              description="三类配置分区彼此独立，按需进入编辑。"
            />
            <div className="grid gap-3 md:grid-cols-3">
              <ConfigEntry
                href={routes.configPosition}
                title="仓位策略"
                description="编辑目标仓位与参考权益"
              />
              <ConfigEntry
                href={routes.configOrder}
                title="下单策略"
                description="编辑 default 执行参数模板"
              />
              <ConfigEntry
                href={routes.configBindings(sourceId)}
                title="策略启用"
                description="为本账户启用策略并发布"
              />
            </div>
          </section>

          {account && report && (
            <AccountMeta account={account} report={report} sourceId={sourceId} />
          )}
        </>
      )}
    </AppShell>
  )
}

function SectionHeading({
  icon: Icon,
  title,
  description,
}: {
  icon: typeof Layers3
  title: string
  description: string
}) {
  return (
    <div className="mb-4 flex items-end justify-between gap-3">
      <div>
        <p className="flex items-center gap-2 text-[11px] font-semibold uppercase tracking-[0.16em] text-brand">
          <Icon size={14} />
          {title}
        </p>
        <p className="mt-1 text-sm text-muted">{description}</p>
      </div>
    </div>
  )
}

function MiniStat({
  label,
  value,
  mono = false,
}: {
  label: string
  value: string
  mono?: boolean
}) {
  return (
    <div className="rounded-xl border border-border-soft bg-surface/80 px-3 py-3">
      <p className="text-[11px] text-subtle">{label}</p>
      <p className={cn('mt-1 text-sm font-medium text-ink', mono && 'truncate font-mono text-[12px]')}>
        {value}
      </p>
    </div>
  )
}

function ConfigEntry({
  href,
  title,
  description,
}: {
  href: string
  title: string
  description: string
}) {
  return (
    <a
      href={href}
      className="group flex items-center justify-between rounded-2xl border border-border bg-surface px-4 py-4 shadow-[var(--shadow-card)] transition hover:border-brand/30 hover:bg-brand-soft/20"
    >
      <div>
        <p className="text-sm font-semibold text-ink">{title}</p>
        <p className="mt-1 text-xs text-muted">{description}</p>
      </div>
      <ArrowRight size={16} className="text-subtle transition group-hover:text-brand" />
    </a>
  )
}

function ActionButton({
  href,
  icon,
  children,
  primary = false,
}: {
  href: string
  icon: React.ReactNode
  children: React.ReactNode
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
      {children}
    </a>
  )
}

function AccountMeta({
  account,
  report,
  sourceId,
}: {
  account: DashboardAccount
  report: SourceNavReport
  sourceId: string
}) {
  return (
    <Card className="mt-8 border-border-soft bg-canvas/30">
      <CardContent className="flex flex-wrap items-center gap-4 py-4 text-sm text-muted">
        <span className="inline-flex items-center gap-2">
          <span className="grid h-8 w-8 place-items-center rounded-lg bg-surface text-xs font-bold text-brand">
            {venueMark(account.venue)}
          </span>
          <code className="text-[11px]">{sourceId}</code>
        </span>
        <span>最近成交 {timestampUs(report.last_fill_ts_us)}</span>
        <span className={signedClass(report.nav_change_after_fee_quote)}>
          累计费后 {money(report.nav_change_after_fee_quote)} USDT
        </span>
      </CardContent>
    </Card>
  )
}
