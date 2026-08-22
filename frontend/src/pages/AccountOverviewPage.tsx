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
import {
  getAccountContractLeverage,
  getAccountStudio,
  getDashboard,
  saveAccountContractLeverage,
  saveAccountFeeRates,
} from '../api'
import { AppShell, PageIntro } from '../components/AppShell'
import {
  ContractLeveragePanel,
  ContractLeverageToolbar,
} from '../components/ContractLeveragePanel'
import { OrderParametersView } from '../components/OrderParametersView'
import { TargetPositionsView } from '../components/TargetPositionsView'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/Card'
import { useStrategyCatalog } from '../hooks/useStrategyCatalog'
import { useConfigWrite } from '../hooks/useConfigWrite'
import { feeBps, money, signedClass, timestampUs } from '../format'
import { FieldHint, Input, Label } from '../components/ui/Field'
import { readSourceId, routes } from '../lib/routes'
import { cn } from '../lib/cn'
import type {
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
  const { saving, error: writeError, notice, withWrite } = useConfigWrite()
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [studio, setStudio] = useState<AccountStudio | null>(null)
  const [contractSymbol, setContractSymbol] = useState('')
  const [contractLeverage, setContractLeverage] = useState('5')
  const [queriedContractLeverage, setQueriedContractLeverage] = useState<string | null>(null)
  const [makerFeeRateInput, setMakerFeeRateInput] = useState('')
  const [takerFeeRateInput, setTakerFeeRateInput] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(async (signal?: AbortSignal) => {
    if (!sourceId) return
    const snapshot = await getDashboard(signal)
    const nextStudio = await getAccountStudio(sourceId, signal)
    setDashboard(snapshot)
    setStudio(nextStudio)
    setMakerFeeRateInput(String(nextStudio.maker_fee_rate))
    setTakerFeeRateInput(String(nextStudio.taker_fee_rate))
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
  const knownLiquidityVolume =
    (report?.maker_volume_quote ?? 0) + (report?.taker_volume_quote ?? 0)
  const makerRatio = knownLiquidityVolume > 0
    ? (report?.maker_volume_quote ?? 0) / knownLiquidityVolume
    : 0
  const takerRatio = knownLiquidityVolume > 0 ? 1 - makerRatio : 0

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
          {writeError && <Alert tone="error" className="mb-4">{writeError}</Alert>}
          {notice && <Alert tone="success" className="mb-4">{notice}</Alert>}

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

          <div className="mb-8 space-y-4">
            <Card>
              <CardHeader className="border-b border-border-soft/80">
                <div>
                  <CardTitle className="text-base">估算费率</CardTitle>
                  <CardDescription className="mt-1">
                    仅用于 Manager NAV / timeline 手续费估算，不改交易所与 Exec 下单。保存后立即写入
                    PostgreSQL，无需重启。
                  </CardDescription>
                </div>
              </CardHeader>
              <CardContent className="space-y-3 pt-5">
                <div className="flex flex-wrap items-end gap-3">
                  <Label className="min-w-[12rem] flex-1">
                    Maker 费率（小数）
                    <Input
                      inputMode="decimal"
                      value={makerFeeRateInput}
                      onChange={(event) => setMakerFeeRateInput(event.target.value)}
                      placeholder="-0.00005"
                      disabled={saving}
                    />
                  </Label>
                  <Label className="min-w-[12rem] flex-1">
                    Taker 费率（小数）
                    <Input
                      inputMode="decimal"
                      value={takerFeeRateInput}
                      onChange={(event) => setTakerFeeRateInput(event.target.value)}
                      placeholder="0.000146"
                      disabled={saving}
                    />
                  </Label>
                  <Button
                    type="button"
                    size="sm"
                    disabled={saving}
                    onClick={() =>
                      void withWrite(async () => {
                        const maker = Number(makerFeeRateInput.trim())
                        const taker = Number(takerFeeRateInput.trim())
                        if (!Number.isFinite(maker) || !Number.isFinite(taker)) {
                          throw new Error('Maker 和 Taker 费率必须是有限数字')
                        }
                        const next = await saveAccountFeeRates(sourceId, maker, taker)
                        setStudio(next)
                        setMakerFeeRateInput(String(next.maker_fee_rate))
                        setTakerFeeRateInput(String(next.taker_fee_rate))
                        await refresh()
                        return `已保存 Maker ${feeBps(next.maker_fee_rate)} / Taker ${feeBps(next.taker_fee_rate)}，NAV 已重算`
                      })
                    }
                  >
                    保存
                  </Button>
                </div>
                <FieldHint>
                  当前 Maker {studio ? `${feeBps(studio.maker_fee_rate)}（${studio.maker_fee_rate}）` : '--'}
                  {' · '}Taker {studio ? `${feeBps(studio.taker_fee_rate)}（${studio.taker_fee_rate}）` : '--'}。
                  负数表示返佣；每笔估算费 = price × qty × 对应流动性费率。
                </FieldHint>
                <div className="space-y-2 border-t border-border-soft pt-3">
                  <div className="flex items-center justify-between text-sm">
                    <span className="font-medium text-ink">成交名义比例</span>
                    <span className="mono text-muted">
                      Maker {(makerRatio * 100).toFixed(2)}% · Taker {(takerRatio * 100).toFixed(2)}%
                    </span>
                  </div>
                  <div className="flex h-2 overflow-hidden bg-surface-strong">
                    <div className="bg-emerald-600" style={{ width: `${makerRatio * 100}%` }} />
                    <div className="bg-amber-500" style={{ width: `${takerRatio * 100}%` }} />
                  </div>
                  <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted">
                    <span>Maker {money(report?.maker_volume_quote ?? 0)} USDT / {report?.maker_fill_count ?? 0} 笔</span>
                    <span>Taker {money(report?.taker_volume_quote ?? 0)} USDT / {report?.taker_fill_count ?? 0} 笔</span>
                    {(report?.unknown_liquidity_fill_count ?? 0) > 0 && (
                      <span>Unknown {money(report?.unknown_liquidity_volume_quote ?? 0)} USDT / {report?.unknown_liquidity_fill_count ?? 0} 笔</span>
                    )}
                  </div>
                </div>
              </CardContent>
            </Card>

            <ContractLeveragePanel
              toolbar={
                <ContractLeverageToolbar
                  symbol={contractSymbol}
                  contractLeverage={contractLeverage}
                  queriedLeverage={queriedContractLeverage}
                  saving={saving}
                  onSymbolChange={(value) => {
                    setContractSymbol(value)
                    setQueriedContractLeverage(null)
                  }}
                  onContractLeverageChange={setContractLeverage}
                  onQuery={() =>
                    void withWrite(async () => {
                      const next = await getAccountContractLeverage(sourceId, contractSymbol)
                      setContractSymbol(next.symbol)
                      setContractLeverage(String(next.contract_leverage))
                      setQueriedContractLeverage(String(next.contract_leverage))
                      const recorded =
                        next.recorded_contract_leverage == null
                          ? '本地无上次设置'
                          : `本地上次设置 ${next.recorded_contract_leverage}x`
                      return `交易所 ${next.symbol} 当前合约杠杆 ${next.contract_leverage}x（${recorded}）`
                    })
                  }
                  onSave={() =>
                    void withWrite(async () => {
                      const next = await saveAccountContractLeverage(
                        sourceId,
                        contractSymbol,
                        Number(contractLeverage),
                      )
                      setContractSymbol(next.symbol)
                      setContractLeverage(String(next.contract_leverage))
                      setQueriedContractLeverage(String(next.contract_leverage))
                      return `已将 ${next.symbol} 合约杠杆设为 ${next.contract_leverage}x`
                    })
                  }
                />
              }
            />
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
                          <Badge tone="brand">{binding.shares} 份</Badge>
                        </div>
                      </CardHeader>
                      <CardContent className="space-y-5 pt-5">
                        <div className="grid gap-3 sm:grid-cols-2">
                          <MiniStat label="仓位策略" value={binding.position_strategy_name} mono />
                          <MiniStat label="执行算法" value={binding.order_strategy_name} mono />
                          <MiniStat label="份数" value={String(binding.shares)} />
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
                description="编辑每份策略的原始目标仓位"
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
