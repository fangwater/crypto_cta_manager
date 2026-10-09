import { ArrowDownToLine, ArrowRight, Check, ChevronDown, Coins, Eye, EyeOff, FlaskConical, LoaderCircle, Play, RefreshCw, ShieldCheck, SlidersHorizontal, Wallet } from 'lucide-react'
import { useEffect, useState } from 'react'
import { acknowledgeBnbOperation, getBnbSettings, runBnbManagement, saveBnbSettings, type BnbSettings, type BnbStatus } from '../api'
import { Alert, Badge } from './ui/Badge'
import { Button } from './ui/Button'
import { Input, Label } from './ui/Field'

const amount = (value: number | null | undefined) => value == null ? '—' : value.toLocaleString('zh-CN', { maximumFractionDigits: 4 })
const time = (ms: number) => new Date(ms).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai', hour12: false })

export function BnbManagement({ sourceId, configurable }: { sourceId: string; configurable: boolean }) {
  const [status, setStatus] = useState<BnbStatus | null>(null)
  const [settings, setSettings] = useState<BnbSettings | null>(null)
  const [token, setToken] = useState('')
  const [visible, setVisible] = useState(false)
  const [busy, setBusy] = useState('')
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [verified, setVerified] = useState(false)

  useEffect(() => {
    const controller = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    async function refresh(initial: boolean) {
      try {
        const next = await getBnbSettings(sourceId, controller.signal)
        if (controller.signal.aborted) return
        setStatus(next)
        setSettings(current => initial || current === null ? next.settings : current)
      } catch (reason) {
        if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : String(reason))
      } finally {
        if (!controller.signal.aborted) timer = setTimeout(() => void refresh(false), 15_000)
      }
    }
    void refresh(true)
    return () => { controller.abort(); clearTimeout(timer) }
  }, [sourceId])
  useEffect(() => setVerified(false), [status?.pending?.at_ms])

  const dirty = !!settings && !!status && JSON.stringify(settings) !== JSON.stringify(status.settings)
  const disabled = !configurable || !!busy
  const valid = !!settings && Object.entries(settings).every(([, v]) => typeof v !== 'number' || Number.isFinite(v))
    && settings.required_bnb > 0 && settings.required_bnb < settings.refill_trigger_bnb && settings.refill_trigger_bnb < settings.refill_target_bnb
    && 1 < settings.futures_trigger_bnb && settings.futures_trigger_bnb < settings.futures_target_bnb && settings.futures_target_bnb < settings.futures_sweep_bnb
    && settings.futures_target_bnb < settings.refill_target_bnb && settings.hedge_tolerance_bnb >= 0.5
    && settings.interval_secs >= 10 && settings.interval_secs <= 3600 && settings.earn_min_bnb > 0
    && Number.isInteger(settings.hedge_min_interval_secs) && settings.hedge_min_interval_secs >= 3600 && settings.hedge_min_interval_secs <= 86400
    && settings.max_conversion_usdt > 0 && settings.max_conversion_usdt <= 1_000_000
    && settings.max_quote_deviation_bps > 0 && settings.max_quote_deviation_bps <= 500

  async function act(action: 'save' | 'preview' | 'run' | 'refresh' | 'acknowledge') {
    if (!settings || (action !== 'refresh' && !token.trim())) return
    setBusy(action); setError(''); setNotice('')
    try {
      if (action === 'save') {
        const next = await saveBnbSettings(sourceId, settings, token)
        setStatus(next); setSettings(next.settings); setNotice('BNB 管理设置已保存')
      } else if (action === 'acknowledge' && status?.pending) {
        const next = await acknowledgeBnbOperation(sourceId, status.pending.at_ms, token)
        setStatus(next); setVerified(false); setNotice('已记录人工核对结果，下轮将重新读取余额')
      } else if (action === 'preview' || action === 'run') {
        const result = await runBnbManagement(sourceId, action, token)
        setNotice(result.result)
        setStatus(await getBnbSettings(sourceId))
      } else {
        const next = await getBnbSettings(sourceId)
        setStatus(next)
        if (!dirty) setSettings(next.settings)
      }
      if (action !== 'refresh') { setToken(''); setVisible(false) }
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason))
      if (action !== 'save') { try { setStatus(await getBnbSettings(sourceId)) } catch { /* retain last status */ } }
    } finally { setBusy('') }
  }

  const b = status?.balances
  const total = b ? b.spot_bnb + b.futures_bnb + b.earn_bnb : null
  const stale = b ? Date.now() - b.at_ms > Math.max(180_000, (status?.settings.interval_secs ?? 60) * 3000) : false
  const label = !status ? '读取中' : status.pending ? '等待确认' : dirty ? '未保存' : !status.settings.enabled ? '未启用' : status.settings.dry_run ? '试运行' : '已启用'
  const tone = status?.pending || dirty ? 'warning' : status?.settings.enabled ? 'brand' : 'neutral'
  const field = (key: keyof BnbSettings, title: string, unit = 'BNB', hint?: string, prominent = false, scale = 1) => settings && (
    <Label className="min-w-0">
      {title}
      <div className="relative">
        <Input type="number" inputMode="decimal" min="0" step="any" value={Number(settings[key]) / scale}
          disabled={disabled} onChange={(event) => setSettings({ ...settings, [key]: Number(event.target.value) * scale })}
          className={`pr-16 tabular-nums ${prominent ? 'h-14 text-2xl font-semibold tracking-tight' : 'h-10'}`} />
        <span className="pointer-events-none absolute inset-y-0 right-3 flex items-center text-[11px] font-medium text-subtle">{unit}</span>
      </div>
      {hint && <span className="text-[11px] font-normal leading-5 text-subtle">{hint}</span>}
    </Label>
  )

  return (
    <section className="mt-6 overflow-hidden rounded-2xl border border-border bg-surface shadow-sm" aria-labelledby="bnb-management-title">
      <div className="flex flex-wrap items-center justify-between gap-4 border-b border-border px-5 py-5 sm:px-7">
        <div className="flex items-center gap-3.5">
          <div className="grid h-11 w-11 shrink-0 place-items-center rounded-xl border border-amber-200 bg-amber-50 text-amber-600"><Coins size={23} strokeWidth={1.6} /></div>
          <div><div className="flex flex-wrap items-center gap-2.5"><h2 id="bnb-management-title" className="text-base font-semibold text-ink">BNB 自动管理</h2><Badge tone={tone}>{label}</Badge></div>
            <p className="mt-1 text-xs text-muted">优先保有 BNB · 独立动态对冲 · 余量活期理财</p></div>
        </div>
        {settings && <label className="flex cursor-pointer items-center gap-3 text-xs font-medium text-muted">
          自动管理
          <input type="checkbox" className="peer sr-only" checked={settings.enabled} disabled={disabled}
            onChange={(event) => setSettings({ ...settings, enabled: event.target.checked })} />
          <span aria-hidden="true" className="relative h-6 w-11 rounded-full bg-border transition-colors after:absolute after:left-1 after:top-1 after:h-4 after:w-4 after:rounded-full after:bg-white after:shadow-sm after:transition-transform peer-checked:bg-brand peer-checked:after:translate-x-5 peer-focus-visible:ring-2 peer-focus-visible:ring-brand-ring peer-disabled:opacity-50" />
        </label>}
      </div>
      {error && <Alert className="mx-5 mt-5 break-words sm:mx-7" role="alert">{error}</Alert>}
      {notice && <Alert tone="success" className="mx-5 mt-5 break-words sm:mx-7" role="status">{notice}</Alert>}
      {!settings ? <div className="flex items-center justify-center gap-2 py-14 text-sm text-muted"><LoaderCircle size={16} className="animate-spin" /> 正在读取 BNB 配置</div> : <>
        <div className="grid grid-cols-2 gap-5 border-b border-border bg-canvas/50 px-5 py-5 sm:grid-cols-4 sm:px-7">
          {([
            ['总持有量', total, 'BNB'], ['合约手续费备用金', b?.futures_bnb, 'BNB'], ['活期理财', b?.earn_bnb, 'BNB'], ['实际对冲仓位', status?.hedge_qty, 'BNB'],
          ] as const).map(([title, value, unit]) => <div key={title} className="min-w-0"><p className="text-[11px] text-muted">{title}</p><p className="mt-1.5 break-words text-xl font-semibold tabular-nums tracking-tight text-ink">{amount(value)} <span className="text-[10px] font-normal tracking-normal text-subtle">{unit}</span></p></div>)}
        </div>
        <div className="grid lg:grid-cols-[minmax(0,1.35fr)_minmax(0,1fr)]">
          <div className="min-w-0 space-y-7 p-5 sm:p-7">
            <div>
              <div className="mb-4 flex items-center gap-2 text-sm font-semibold text-ink"><ShieldCheck size={16} className="text-brand" /> 持币与补足</div>
              <div className="mb-5 max-w-xs">{field('required_bnb', '目标 VIP 所需 BNB', 'BNB', '按账户目标等级填写，不随当前等级自动下调。')}</div>
              <div className="rounded-xl border border-brand-ring/50 bg-brand-soft/30 p-4 sm:p-5">
                <div className="grid items-center gap-3 sm:grid-cols-[minmax(0,1fr)_24px_minmax(0,1fr)]">
                  {field('refill_trigger_bnb', '总量低于或等于', 'BNB', undefined, true)}
                  <ArrowRight size={20} className="mx-auto rotate-90 text-brand sm:mt-5 sm:rotate-0" aria-hidden="true" />
                  {field('refill_target_bnb', '一次补足至', 'BNB', undefined, true)}
                </div>
                <p className="mt-4 text-xs leading-6 text-muted">触及 <strong className="font-semibold text-ink">{amount(settings.refill_trigger_bnb)} BNB</strong> 才兑换，补到 <strong className="font-semibold text-ink">{amount(settings.refill_target_bnb)} BNB</strong> 后停止，减少小额操作。</p>
              </div>
              <div className="mt-3 flex items-start gap-2 text-[11px] leading-5 text-subtle"><ArrowDownToLine size={14} className="mt-0.5 shrink-0" /><span>始终使用 BFUSD → BNB 闪兑，独立于 USDT 理财运行。价格对冲不阻塞补足。</span></div>
            </div>
            <div>
              <div className="mb-4 flex items-center gap-2 text-sm font-semibold text-ink"><Wallet size={16} className="text-brand" /> 合约手续费备用金</div>
              <div className="grid gap-4 sm:grid-cols-2">{field('futures_trigger_bnb', '余额补充下限')}{field('futures_target_bnb', '补充后余额')}</div>
              <p className="mt-3 text-xs leading-6 text-subtle">优先从已有 BNB 调拨，剩余存入活期。钱包间划转不触发重复买币。</p>
            </div>
          </div>
          <aside className="min-w-0 border-t border-border bg-canvas/30 p-5 sm:p-7 lg:border-l lg:border-t-0">
            <div className="mb-4 flex items-center justify-between"><h3 className="text-sm font-semibold text-ink">运行状态</h3><Button size="sm" variant="ghost" disabled={!!busy} onClick={() => void act('refresh')} aria-label="刷新 BNB 状态"><RefreshCw size={14} className={busy === 'refresh' ? 'animate-spin' : ''} /></Button></div>
            <dl className="space-y-4 text-xs">
              <div className="flex justify-between gap-3"><dt className="text-muted">当前对冲合约</dt><dd className="font-medium text-ink">{status?.hedge_symbol || '—'} 永续</dd></div>
              {status?.legacy_hedge_qty != null && Math.abs(status.legacy_hedge_qty) > 0.00000001 && <div className="flex justify-between gap-3"><dt className="text-muted">旧 BNBUSDT 对冲尾差</dt><dd className="font-medium tabular-nums text-ink">{amount(status.legacy_hedge_qty)} BNB</dd></div>}
              <div className="flex justify-between gap-3"><dt className="text-muted">BFUSD 持有余额</dt><dd className="text-right font-medium tabular-nums text-ink">{b ? amount(b.spot_bfusd + b.futures_bfusd) : '—'} BFUSD</dd></div>
              <div className="flex justify-between gap-3"><dt className="text-muted">现货 BNB</dt><dd className="font-medium tabular-nums text-ink">{amount(b?.spot_bnb)} BNB</dd></div>
              <div className="flex justify-between gap-3"><dt className="text-muted">对冲调整阈值</dt><dd className="font-medium tabular-nums text-ink">{amount(settings.hedge_tolerance_bnb)} BNB</dd></div>
              <div className="flex justify-between gap-3"><dt className="text-muted">对冲最小调整间隔</dt><dd className="font-medium tabular-nums text-ink">{amount(settings.hedge_min_interval_secs / 3600)} 小时</dd></div>
              <div className="flex justify-between gap-3"><dt className="text-muted">当前净敞口</dt><dd className="font-medium tabular-nums text-ink">{amount(total != null && (status?.hedge_qty != null || status?.legacy_hedge_qty != null) ? total + (status?.hedge_qty ?? 0) + (status?.legacy_hedge_qty ?? 0) : null)} BNB</dd></div>
            </dl>
            <p className="mt-4 text-[11px] leading-5 text-subtle">BNBUSDC 专用于 BNB 储备对冲，普通 CTA 禁用。实际净敞口达到阈值且达到最小间隔才调整；调仓触发阈值不影响成交精度。多资产模式下仍与 CTA 共享保证金。</p>
            <div className="mt-6 border-t border-border pt-5">
              <p className="text-[11px] font-medium text-muted">最近检查</p>
              <p className="mt-2 break-words text-xs leading-6 text-ink">{status?.last_result || '尚无检查记录，可先运行检查读取账户余额。'}</p>
              {b && <p className="mt-3 text-[10px] tabular-nums text-subtle">{time(b.at_ms)}（上海时间）{stale ? ' · 余额待更新' : ''}</p>}
            </div>
            {status?.hedge_error && <Alert tone="warning" className="mt-4 break-words">对冲待恢复：{status.hedge_error}</Alert>}
            {status?.pending && <div className="mt-4 rounded-xl border border-amber-200 bg-amber-50 p-4 text-xs leading-6 text-amber-900">
              <p className="font-semibold">正在等待上一笔操作确认</p><p className="break-words">{status.pending.action}</p><p>确认结果前不会重复提交。仅在人工核对交易所记录、无在途操作且余额一致后解除。</p>
              {configurable && <><label className="mt-3 flex items-start gap-2"><input type="checkbox" className="mt-1.5" checked={verified} disabled={!!busy} onChange={(e) => setVerified(e.target.checked)} />已核对交易所最终结果及余额</label><Button className="mt-3" size="sm" disabled={!verified || !token || !!busy} onClick={() => void act('acknowledge')}>记录核对并解除等待</Button></>}
            </div>}
          </aside>
        </div>
        <details className="group border-t border-border px-5 sm:px-7">
          <summary className="flex cursor-pointer list-none items-center gap-2 py-4 text-xs font-medium text-muted [&::-webkit-details-marker]:hidden"><SlidersHorizontal size={15} /> 高级参数 <ChevronDown size={15} className="ml-auto transition-transform group-open:rotate-180" /></summary>
          <div className="grid gap-4 pb-6 sm:grid-cols-2 xl:grid-cols-3">
            {field('futures_sweep_bnb', '合约余额转出上限')}{field('earn_min_bnb', '活期最小申购量')}{field('hedge_tolerance_bnb', '对冲调整阈值', 'BNB', '至少 0.5 BNB；与 VIP 补仓阈值独立。')}
            {field('interval_secs', '检查间隔', '秒')}{field('max_conversion_usdt', '单次兑换金额上限', 'USDT')}{field('max_quote_deviation_bps', '最大报价偏差', 'bps')}
            {field('hedge_min_interval_secs', '对冲最小调整间隔', '小时', '1–24 小时；检查余额不代表每次都调仓。', false, 3600)}
          </div>
        </details>
        {!valid && <Alert tone="warning" className="mx-5 mb-5 sm:mx-7">请检查参数：VIP 门槛 &lt; 补买触发量 &lt; 补足目标量；手续费备用金需满足 1 &lt; 下限 &lt; 目标 &lt; 转出上限。对冲间隔为 1–24 小时，调整阈值至少 0.5 BNB；余额检查间隔为 10–3600 秒，报价偏差不超过 500 bps。</Alert>}
        {configurable && <div className="space-y-5 border-t border-border bg-canvas/40 px-5 py-5 sm:px-7">
          <div className="flex flex-wrap items-center justify-between gap-4">
            <div><p className="text-xs font-semibold text-ink">执行模式</p><p className="mt-1 text-[11px] text-subtle">试运行只检查；关闭自动管理会保留当前对冲仓位。</p></div>
            <div className="inline-flex shrink-0 rounded-lg border border-border bg-surface p-1" role="group" aria-label="BNB 执行模式">
              {[true, false].map((dry) => <button key={String(dry)} type="button" disabled={disabled} aria-pressed={settings.dry_run === dry} onClick={() => setSettings({ ...settings, dry_run: dry })} className={`flex items-center gap-1.5 rounded-md px-3 py-2 text-xs font-medium transition-colors disabled:opacity-50 ${settings.dry_run === dry ? 'bg-brand-soft text-brand-hover shadow-sm' : 'text-muted hover:bg-canvas'}`}>{dry ? <FlaskConical size={14} /> : <Play size={14} />}{dry ? '试运行' : '实际执行'}</button>)}
            </div>
          </div>
          <div className="flex flex-col gap-4 sm:flex-row sm:items-end sm:justify-between">
            <Label className="w-full sm:max-w-xs">操作 token<div className="relative"><Input type={visible ? 'text' : 'password'} autoComplete="off" value={token} onChange={(e) => setToken(e.target.value)} disabled={!!busy} className="pr-10" placeholder="与自动理财使用同一操作 token" /><button type="button" onClick={() => setVisible(!visible)} aria-label={visible ? '隐藏 token' : '显示 token'} className="absolute inset-y-0 right-0 grid w-10 place-items-center text-muted">{visible ? <EyeOff size={15} /> : <Eye size={15} />}</button></div></Label>
            <div className="flex flex-wrap gap-2">
              <Button disabled={disabled || !token || dirty} onClick={() => void act('preview')}><FlaskConical size={15} /> 运行检查</Button>
              <Button disabled={disabled || !token || dirty || !settings.enabled} onClick={() => void act('run')}><Play size={15} /> 执行一轮</Button>
              <Button variant="primary" disabled={disabled || !token || !dirty || !valid} onClick={() => void act('save')}>{busy === 'save' ? <LoaderCircle size={15} className="animate-spin" /> : <Check size={15} />} 保存配置</Button>
            </div>
          </div>
        </div>}
      </>}
    </section>
  )
}
