import { accountExecutionGroup, estimatedFundingOnly } from '../lib/executionGroups'
import { accountBindings } from '../lib/accountBindings'
import { accountScope } from '../lib/accountScope'
import { accountVenueLabel } from '../lib/workspace'
import { AccountFeeEditor } from '../components/AccountFeeEditor'
import { FundingSettingsPanel } from '../components/FundingSettingsPanel'
import { FundingStatusBadge } from '../components/FundingStatusBadge'
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
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  getAccountContractLeverage,
  getAccountStudio,
  getMultiMarketAccountStudio,
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
import { Select } from '../components/ui/Field'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/Card'
import { useStrategyCatalog } from '../hooks/useStrategyCatalog'
import { useConfigWrite } from '../hooks/useConfigWrite'
import { executionMarketLabel, executionSymbolPlaceholder, formatUniMmr, isUnifiedVenue, money, signedClass, timestampUs } from '../format'
import { readSourceId, routes } from '../lib/routes'
import { accountSignalConfigPath } from '../lib/signalConfigLinks'
import { cn } from '../lib/cn'
import { usesActualFunding } from '../lib/fundingPresentation'
import type {
  AccountStudio,
  DashboardAccount,
  DashboardSnapshot,
  SourceNavReport,
} from '../types'

function uniMmrTone(value: number | null | undefined, status?: string | null) {
  if (status === 'stale' || value == null || !Number.isFinite(value)) return 'text-muted'
  if (value <= 1.05) return 'text-rose-700'
  if (value <= 1.5) return 'text-warning'
  return 'text-ink'
}

function venueMark(venue: string) {
  const normalized = venue.toLowerCase()
  if (normalized.includes('binance')) return 'B'
  return venue.slice(0, 2).toUpperCase()
}

export function AccountOverviewPage() {
  const accountId = readSourceId()
  const selectedMarket = useRef('')
  const [sourceId, setSourceId] = useState(accountId)
  const { positions, orders, loading: catalogLoading } = useStrategyCatalog()
  const { saving, error: writeError, notice, withWrite } = useConfigWrite()
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [studio, setStudio] = useState<AccountStudio | null>(null)
  const [accountStudios, setAccountStudios] = useState<AccountStudio[]>([])
  const [contractSymbol, setContractSymbol] = useState('')
  const [contractLeverage, setContractLeverage] = useState('5')
  const [queriedContractLeverage, setQueriedContractLeverage] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(async (signal?: AbortSignal) => {
    if (!accountId) return
    const snapshot = await getDashboard(signal)
    const scope = accountScope(snapshot.accounts ?? [], accountId)
    if (!scope) throw new Error('账户不可见或不存在')
    const markets = scope.account.execution_sources ?? []
    const nativeId = markets.find(s => s.source_id === selectedMarket.current)?.source_id
      ?? scope.execution?.source_id ?? markets[0]?.source_id ?? scope.account.source_id
    const studios = markets.length
      ? (await getMultiMarketAccountStudio(scope.account.source_id, signal)).execution_sources
      : [await getAccountStudio(nativeId, signal)]
    selectedMarket.current = nativeId
    setSourceId(nativeId)
    setDashboard(snapshot)
    setAccountStudios(studios)
    setStudio(studios.find(s => s.source_id === nativeId) ?? null)
    window.history.replaceState({}, '', routes.account(scope.account.source_id))
  }, [accountId])

  useEffect(() => {
    if (!accountId) {
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
  }, [refresh, accountId])

  const account = useMemo(() => (dashboard?.accounts ?? []).find(a => a.source_id === accountId || a.execution_sources?.some(s => s.source_id === sourceId)), [dashboard, accountId, sourceId])
  const publicId = account?.source_id ?? accountId
  const executionGroup = account ? accountExecutionGroup(account) : null
  const executionUrl = executionGroup ? routes.execViz(executionGroup.id) : account?.gateway_prefix ? `${account.gateway_prefix}/` : null
  const markets = account?.execution_sources ?? []
  const marketAccount = markets.find(a => a.source_id === sourceId) ?? account
  const report = dashboard?.report.sources.find(s => s.source_id === publicId)
  const bindings = useMemo(() => accountBindings(accountStudios), [accountStudios])
  const liquidityReport = report?.execution_sources?.find(s => s.source_id === sourceId) ?? report
  const canConfigure = marketAccount?.access_level === 'configure'
  const signalConfigPath = canConfigure ? accountSignalConfigPath(publicId) : undefined
  const knownLiquidityVolume =
    (liquidityReport?.maker_volume_quote ?? 0) + (liquidityReport?.taker_volume_quote ?? 0)
  const makerRatio = knownLiquidityVolume > 0
    ? (liquidityReport?.maker_volume_quote ?? 0) / knownLiquidityVolume
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
      <AppShell active="accounts" title="账户配置" subtitle="只读概览" icon={WalletCards}>
        <Alert tone="warning">缺少 source 参数。请从账户页进入某个账户。</Alert>
        <a href={routes.accounts} className="mt-4 inline-flex text-sm font-medium text-brand">
          返回账户列表
        </a>
      </AppShell>
    )
  }

  return (
    <AppShell
      active="accounts"
      title={account?.account ?? accountId}
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
            title={account?.account ?? accountId}
            description={
              markets.length ? '在同一账户中查看整体盈亏与策略，按交易市场设置费率和执行参数。' : isUnifiedVenue(account?.venue ?? '')
                ? '统一账户权益包含账户全部抵押资产。UniMMR 为保证金覆盖倍数，数值越大越安全；无可用比率时显示 --。'
                : '这里只展示当前组合配置与运行摘要。修改请进入右侧独立的策略配置分区。'
            }
            actions={
              <div className="flex flex-wrap items-center gap-2">
                {(markets.length > 0 || estimatedFundingOnly(sourceId)) && <Badge tone="neutral">Funding：预估</Badge>}
                <ActionButton href={routes.nav(publicId)} primary icon={<Activity size={15} />}>
                  净值
                </ActionButton>
                {executionUrl && (
                  <ActionButton href={executionUrl} icon={<ExternalLink size={15} />}>
                    Exec Viz
                  </ActionButton>
                )}
                {canConfigure && (
                  <ActionButton
                    href={routes.configBindings(publicId)}
                    icon={<PencilLine size={15} />}
                  >
                    编辑启用
                  </ActionButton>
                )}
                {signalConfigPath && (
                  <ActionButton href={signalConfigPath} icon={<SlidersHorizontal size={15} />}>
                    信号配置
                  </ActionButton>
                )}
              </div>
            }
          />

          <div className="mb-8 space-y-4">
            {markets.length > 0 && <Card><CardContent className="flex flex-wrap items-center gap-3 pt-5">
              <label className="text-sm font-medium text-ink" htmlFor="account-market">交易市场</label>
              <Select id="account-market" aria-label="交易市场" value={sourceId} disabled={saving} className="max-w-xs" onChange={event => {
                const next = event.target.value
                if (!markets.some(m => m.source_id === next)) return
                selectedMarket.current = next
                setSourceId(next)
                setStudio(accountStudios.find(s => s.source_id === next) ?? null)
                setContractSymbol('')
                setQueriedContractLeverage(null)
              }}>{markets.map(m => <option key={m.source_id} value={m.source_id}>{accountVenueLabel(m.venue)}</option>)}</Select>
            </CardContent></Card>}
            {markets.length === 0 && !estimatedFundingOnly(sourceId) && marketAccount?.venue === 'binance-futures' && <FundingSettingsPanel key={sourceId} sourceId={sourceId} canConfigure={canConfigure} />}
            <Card>
              <CardHeader className="border-b border-border-soft/80">
                <div>
                  <CardTitle className="text-base">估算费率</CardTitle>
                  <CardDescription className="mt-1">
                    按账户和合约报价币设置成交及理论成交 费率。未单独设置的报价币使用默认费率。保存后自动更新。
                  </CardDescription>
                </div>
              </CardHeader>
              <CardContent className="space-y-3 pt-5">
                {studio && <AccountFeeEditor studio={studio} disabled={saving || !canConfigure}
                  onSave={(maker, taker, theory, quotes) => void withWrite(async () => {
                    const next = await saveAccountFeeRates(sourceId, maker, taker, theory, quotes)
                    setStudio(next)
                    await refresh()
                    return '已保存报价币费率；成交手续费重新估算，理论费率对后续目标生效'
                  })} />}
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
                    <span>Maker {money(liquidityReport?.maker_volume_quote ?? 0)} 报价币 / {liquidityReport?.maker_fill_count ?? 0} 笔</span>
                    <span>Taker {money(liquidityReport?.taker_volume_quote ?? 0)} 报价币 / {liquidityReport?.taker_fill_count ?? 0} 笔</span>
                    {(liquidityReport?.unknown_liquidity_fill_count ?? 0) > 0 && (
                      <span>Unknown {money(liquidityReport?.unknown_liquidity_volume_quote ?? 0)} 报价币 / {liquidityReport?.unknown_liquidity_fill_count ?? 0} 笔</span>
                    )}
                  </div>
                </div>
              </CardContent>
            </Card>

            <ContractLeveragePanel
              toolbar={
                <ContractLeverageToolbar
                  symbolPlaceholder={executionSymbolPlaceholder(marketAccount?.venue ?? '')}
                  symbol={contractSymbol}
                  contractLeverage={contractLeverage}
                  queriedLeverage={queriedContractLeverage}
                  saving={saving}
                  readOnly={!canConfigure}
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
              description={`${bindings.length} 条策略 · 管理账户内各市场的执行参数`}
            />
            {bindings.length === 0 ? (
              <Card>
                <CardContent className="py-12 text-center text-sm text-muted">
                  尚未绑定策略组合。
                  <a href={routes.configBindings(publicId)} className="ml-1 font-medium text-brand">
                    去配置
                  </a>
                </CardContent>
              </Card>
            ) : (
              <div className="grid gap-4 xl:grid-cols-2">
                {bindings.map((binding) => {
                  const position = positionByName.get(binding.position_strategy_name)
                  const order = orderByName.get(binding.order_strategy_name)
                  const signalConfigPath = canConfigure
                    ? accountSignalConfigPath(publicId, binding.position_strategy_name)
                    : undefined
                  return (
                    <Card
                      key={`${binding.binding_name}:${binding.position_strategy_name}`}
                      className="overflow-hidden border-border-soft bg-gradient-to-br from-surface to-canvas/40"
                    >
                      <CardHeader className="border-b border-border-soft/80">
                        <div className="flex items-start justify-between gap-3">
                          <div>
                            <CardTitle className="text-base">{binding.binding_name}</CardTitle>
                            <CardDescription className="mt-1 font-mono text-[11px]">
                              策略名
                            </CardDescription>
                          </div>
                          <Badge tone={binding.shares === 0 ? 'neutral' : 'brand'}>
                            {binding.shares === 0 ? '已停止' : binding.shares == null ? '按市场配置' : `${binding.shares} 份`}
                          </Badge>
                        </div>
                      </CardHeader>
                      <CardContent className="space-y-5 pt-5">
                        {signalConfigPath && (
                          <ActionButton href={signalConfigPath} icon={<SlidersHorizontal size={15} />}>
                            信号配置
                          </ActionButton>
                        )}
                        <div className="grid gap-3 sm:grid-cols-2">
                          <MiniStat label="仓位策略" value={binding.position_strategy_name} mono />
                          <MiniStat label="执行算法" value={binding.order_strategy_name} mono />
                          <MiniStat label="份数" value={binding.shares == null ? '按市场配置' : String(binding.shares)} />
                        </div>
                        {markets.length > 0 && <div className="space-y-2 text-sm">{binding.markets.map(item => <div key={item.source_id} className="flex flex-wrap gap-x-4 gap-y-1 text-muted">
                          <span>{accountVenueLabel(markets.find(m => m.source_id === item.source_id)?.venue ?? '')}</span>
                          <span>{item.order_strategy_name}</span><span>{item.shares} 份</span>
                        </div>)}</div>}
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
                href={routes.configBindings(publicId)}
                title="策略启用"
                description="为本账户启用策略并发布"
              />
            </div>
          </section>

          {account && report && (
            <AccountMeta account={account} report={report} sourceId={publicId} />
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
        <Badge tone={account.venue === 'binance-coin-futures' ? 'warning' : 'brand'}>{executionMarketLabel(account.venue)}</Badge>
          {account.venue === 'binance-futures' && <FundingStatusBadge value={account.funding} />}
        {(account.unified_account || isUnifiedVenue(account.venue)) && (
          <Badge tone={account.unified_account ? 'brand' : 'neutral'}>统一账户</Badge>
        )}
        {(account.unified_account || isUnifiedVenue(account.venue)) && (
          <span className={uniMmrTone(account.uni_mmr, account.uni_mmr_status)}>
            UniMMR{' '}
            {account.uni_mmr_status === 'stale'
              ? `${formatUniMmr(account.uni_mmr)} 延迟`
              : formatUniMmr(account.uni_mmr)}
          </span>
        )}
        <span>最近成交 {timestampUs(report.last_fill_ts_us)}</span>
        <span className={signedClass(report.nav_change_after_fee_quote)}>
          累计费后 {money(report.nav_change_after_fee_quote)} 报价币
        </span>
        {account.funding && usesActualFunding(account.funding) && <>
          <span className={signedClass(account.funding.income_quote)}>累计实际 Funding {money(account.funding.income_quote)} 报价币</span>
          <span className={signedClass(account.total_nav_after_fee_quote ?? 0)}>总体费后 {money(account.total_nav_after_fee_quote ?? 0)} 报价币</span>
        </>}
      </CardContent>
    </Card>
  )
}
