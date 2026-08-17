import { BookOpen, ChevronLeft, ChevronRight, Globe, Settings2, Target, Terminal } from 'lucide-react'
import { useEffect, useMemo, useState, type ReactNode } from 'react'
import { AppShell } from '../components/AppShell'
import {
  Callout,
  CodeBlock,
  Endpoint,
  FormulaGrid,
  MethodBadge,
  QuickLinks,
  StatusTable,
  Steps,
  type HttpMethod,
} from '../components/docs/DocsPrimitives'
import { cn } from '../lib/cn'

const GATEWAY = 'http://172.16.30.42:10041'
const TRADE01_CONFIG = `${GATEWAY}/exec_trade01/config`
const MANAGER_ACCOUNT = `${GATEWAY}/manager/api/catalog/accounts/binance_exec_trade01`

type ChapterId =
  | 'overview'
  | 'model'
  | 'studio'
  | 'entry'
  | 'op-params'
  | 'op-targets'
  | 'op-full'
  | 'api-targets'
  | 'api-params'
  | 'api-leverage'
  | 'api-allocations'
  | 'api-shares'
  | 'api-strategy'
  | 'api-read'
  | 'api-errors'
  | 'client'

interface Chapter {
  id: ChapterId
  group: string
  title: string
  lead: string
  content: ReactNode
}

function FieldTable({
  rows,
}: {
  rows: Array<{ field: string; detail: ReactNode }>
}) {
  return (
    <div className="not-prose my-5 overflow-hidden rounded-2xl border border-border">
      <table className="w-full text-sm">
        <thead className="bg-canvas text-left text-xs text-muted">
          <tr>
            <th className="px-4 py-2.5 font-medium">字段</th>
            <th className="px-4 py-2.5 font-medium">说明</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.field} className="border-t border-border-soft">
              <td className="px-4 py-3 align-top">
                <code className="rounded-md bg-canvas px-1.5 py-0.5 font-mono text-[12px]">{row.field}</code>
              </td>
              <td className="px-4 py-3 text-[13px] leading-6 text-ink">{row.detail}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function RouteTable({
  rows,
}: {
  rows: Array<{ method?: HttpMethod; label: string; path: ReactNode }>
}) {
  return (
    <div className="not-prose my-5 overflow-hidden rounded-2xl border border-border">
      {rows.map((row) => (
        <div
          key={row.label}
          className="flex flex-wrap items-center gap-3 border-b border-border-soft px-4 py-3 last:border-b-0"
        >
          {row.method ? <MethodBadge method={row.method} /> : null}
          <div className="min-w-32 text-sm font-medium text-ink">{row.label}</div>
          <div className="min-w-0 flex-1 font-mono text-[12.5px] text-muted">{row.path}</div>
        </div>
      ))}
    </div>
  )
}

const chapters: Chapter[] = [
  {
    id: 'overview',
    group: '开始',
    title: '概述',
    lead: '仓位策略和下单策略在 Manager 独立配置，绑定到账户后再发布到 Exec。',
    content: (
      <>
        <QuickLinks
          items={[
            {
              href: '/manager/config/position/',
              title: '浏览器配置',
              detail: '创建仓位、执行算法，再绑定到账户。',
              icon: Settings2,
            },
            {
              href: '#op-targets',
              title: '脚本推仓位',
              detail: 'POST /exec_trade01/config/api/targets',
              icon: Target,
            },
            {
              href: '#op-full',
              title: '参数 + 仓位',
              detail: 'POST /exec_trade01/config/api/strategy',
              icon: Terminal,
            },
            {
              href: '/',
              title: '综合总览',
              detail: '当前入口与账户状态。',
              icon: Globe,
            },
          ]}
        />
        <Callout>当前已部署账户只有 trade01。trade02 会使用独立前缀，不能和 trade01 共用入口。</Callout>
      </>
    ),
  },
  {
    id: 'model',
    group: '开始',
    title: '账户与策略',
    lead: '账户是 URL 前缀，策略是 JSON 里的 strategy_name，不要混用。',
    content: (
      <>
        <div className="not-prose my-5 grid gap-3 sm:grid-cols-2">
          <div className="rounded-2xl border border-border bg-canvas/70 p-4">
            <p className="text-[11px] font-semibold uppercase tracking-[0.14em] text-brand">账户</p>
            <p className="mt-2 font-mono text-sm text-ink">trade01 / trade02</p>
            <p className="mt-2 text-xs leading-5 text-muted">放在路径里，例如 /exec_trade01/</p>
          </div>
          <div className="rounded-2xl border border-border bg-canvas/70 p-4">
            <p className="text-[11px] font-semibold uppercase tracking-[0.14em] text-brand">策略</p>
            <p className="mt-2 break-all font-mono text-sm text-ink">CTA_SK_…_Position</p>
            <p className="mt-2 text-xs leading-5 text-muted">放在 JSON 的 strategy_name</p>
          </div>
        </div>
        <p>
          每个策略自己有一份 target 和一份 Exec 实际仓位。同一账户里，两个策略的 BTC target 可以不同。
        </p>
        <p>
          Exec 仍按「账户 + 策略名」落 Redis。Manager 把仓位策略和下单策略拆开，组合后再发布成 Exec 策略名。
        </p>
      </>
    ),
  },
  {
    id: 'studio',
    group: '开始',
    title: '策略组合',
    lead: '账户只负责杠杆和绑定。权益金额是仓位策略的参考尺度。',
    content: (
      <>
        <div className="not-prose my-5 overflow-hidden rounded-2xl border border-border">
          <table className="w-full text-sm">
            <thead className="bg-canvas text-left text-xs text-muted">
              <tr>
                <th className="px-4 py-2.5 font-medium">对象</th>
                <th className="px-4 py-2.5 font-medium">配置什么</th>
                <th className="px-4 py-2.5 font-medium">默认</th>
              </tr>
            </thead>
            <tbody>
              <tr className="border-t border-border-soft">
                <td className="px-4 py-3 font-medium">仓位策略</td>
                <td className="px-4 py-3 text-muted">策略名、equity_usdt、targets</td>
                <td className="px-4 py-3">10,000 USDT</td>
              </tr>
              <tr className="border-t border-border-soft">
                <td className="px-4 py-3 font-medium">下单策略</td>
                <td className="px-4 py-3 text-muted">策略名和 8 个下单参数</td>
                <td className="px-4 py-3">与 Exec 默认一致</td>
              </tr>
              <tr className="border-t border-border-soft">
                <td className="px-4 py-3 font-medium">账户</td>
                <td className="px-4 py-3 text-muted">杠杆率</td>
                <td className="px-4 py-3">1</td>
              </tr>
              <tr className="border-t border-border-soft">
                <td className="px-4 py-3 font-medium">绑定</td>
                <td className="px-4 py-3 text-muted">仓位策略 × 下单策略</td>
                <td className="px-4 py-3">无</td>
              </tr>
            </tbody>
          </table>
        </div>
        <FormulaGrid
          items={[
            { label: '可用名义', value: '实时权益 × 杠杆率' },
            { label: '已配置名义', value: 'Σ(份数 × 该策略参考权益)' },
            { label: '占比', value: '按已配置名义加权，合计 = 100%' },
            { label: '发布仓位', value: 'target × 份数' },
          ]}
        />
        <Callout>
          各策略的单份参考权益可以不同，容量按名义金额聚合，不按统一 10,000
          折成份数。权益来自本机 account_monitor。未点「发布到 Exec」前，组合只存在 Manager 本地。
        </Callout>
        <p>发布规则：</p>
        <ul>
          <li>Exec 上还没有这个名字：走 POST /api/strategy</li>
          <li>已经存在：分别走 POST /api/targets 和 POST /api/order-parameters</li>
        </ul>
      </>
    ),
  },
  {
    id: 'entry',
    group: '开始',
    title: '入口地址',
    lead: '对外入口是网关。脚本必须带账户前缀。',
    content: (
      <>
        <CodeBlock label="trade01 Config 基址">{`${TRADE01_CONFIG}/`}</CodeBlock>
        <RouteTable
          rows={[
            { label: '综合总览', path: <a href="/">{GATEWAY}/</a> },
            { label: '净值中心', path: <a href="/manager/">{GATEWAY}/manager/</a> },
            {
              label: '策略配置',
              path: <a href="/manager/config/position/">{GATEWAY}/manager/config/position/</a>,
            },
            { label: 'Exec Config API', path: <code>{TRADE01_CONFIG}/api/...</code> },
            { label: 'Exec Viz', path: <code>{GATEWAY}/exec_trade01/</code> },
          ]}
        />
        <Callout tone="warning">
          不要使用 {GATEWAY}/config/，它无法区分 trade01 / trade02。
        </Callout>
      </>
    ),
  },
  {
    id: 'op-params',
    group: '操作方法',
    title: '浏览器配置',
    lead: '下单参数属于独立的下单策略。Exec Config 页面保持只读。',
    content: (
      <>
        <Steps
          items={[
            <>
              打开 <a href="/manager/config/position/">策略配置</a>
            </>,
            '在「仓位策略」里编辑 target 和权益金额',
            '在「下单策略」里编辑 8 个下单参数',
            '在「账户组合」里保存占比，合计等于 100%',
            '点「发布到 Exec」',
          ]}
        />
        <p>8 个下单参数：</p>
        <p>
          <code>single_order_usdt</code>、<code>orders_per_batch</code>、
          <code>maker_price_anchor</code>、<code>tick_spacing</code>、
          <code>batch_interval_ms</code>、<code>maker_timeout_ms</code>、
          <code>max_maker_requotes</code>、<code>target_tolerance_usdt</code>
        </p>
      </>
    ),
  },
  {
    id: 'op-targets',
    group: '操作方法',
    title: '推送目标仓位',
    lead: '只打 POST /api/targets。策略必须已经存在，不会顺手创建算法。',
    content: (
      <>
        <Callout>targets 是整表替换。没写到的品种会从该策略 target 里消失。</Callout>
        <CodeBlock label="exec_config_client.py">{`python3 exec_config_client.py \\
  --url ${TRADE01_CONFIG}/ \\
  post-targets @targets.json`}</CodeBlock>
        <CodeBlock label="targets.json">{`{
  "strategy_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
  "targets": {
    "BTCUSDT": -0.006,
    "ETHUSDT": -0.54
  }
}`}</CodeBlock>
        <p>
          同一账户下另一个策略要另发一次，换 <code>strategy_name</code>。trade02 部署后把{' '}
          <code>--url</code> 改成 <code>/exec_trade02/config/</code>。
        </p>
      </>
    ),
  },
  {
    id: 'op-full',
    group: '操作方法',
    title: '同时推送参数和仓位',
    lead: 'POST /api/strategy 一次提交下单参数和 target。',
    content: (
      <>
        <p>新建策略时两者都会写入；策略已存在时只更新 target，避免覆盖 Manager 改过的参数。</p>
        <p>
          只改仓位用 <code>/api/targets</code>，只改下单参数用 Manager。
        </p>
        <CodeBlock label="exec_config_client.py">{`python3 exec_config_client.py \\
  --url ${TRADE01_CONFIG}/ \\
  post @strategy.json`}</CodeBlock>
      </>
    ),
  },
  {
    id: 'api-targets',
    group: 'API',
    title: '改目标仓位',
    lead: '改某个已有策略的目标仓位，不需要 token。',
    content: (
      <>
        <Endpoint method="POST" path={`${TRADE01_CONFIG}/api/targets`} />
        <CodeBlock label="curl">{`curl --noproxy '*' -X POST ${TRADE01_CONFIG}/api/targets \\
  -H 'Content-Type: application/json' \\
  -d '{
    "strategy_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
    "targets": {"BTCUSDT": -0.006, "ETHUSDT": -0.54}
  }'`}</CodeBlock>
        <FieldTable
          rows={[
            { field: 'strategy_name', detail: '已存在的策略名' },
            { field: 'targets', detail: '品种到数量的对象，数量可为 0 或负数。整表替换。' },
          ]}
        />
        <p>
          成功返回 <code>strategy_name</code>、<code>targets</code>、<code>updated_at_us</code>。未知策略返回 400：
          <code>strategy is not active</code>。
        </p>
      </>
    ),
  },
  {
    id: 'api-params',
    group: 'API',
    title: '改下单参数',
    lead: '只改下单参数，不改仓位。必须带写权限 token，且策略必须已存在。',
    content: (
      <>
        <Endpoint
          method="POST"
          path={`${TRADE01_CONFIG}/api/order-parameters`}
          note="Authorization: Bearer <token>。浏览器请走 Manager。"
        />
        <CodeBlock label="JSON">{`{
  "strategy_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
  "expected_updated_at_us": 1786683845088869,
  "order_parameters": {
    "single_order_usdt": 100.0,
    "orders_per_batch": 3,
    "maker_price_anchor": "own_best",
    "tick_spacing": 1,
    "batch_interval_ms": 500,
    "maker_timeout_ms": 1000,
    "max_maker_requotes": 2,
    "target_tolerance_usdt": 10.0
  }
}`}</CodeBlock>
        <Callout tone="warning">
          只接受上述 8 个参数字段。带 targets 会被拒绝。版本不一致返回 409，需要重新 GET 后再写。
        </Callout>
      </>
    ),
  },
  {
    id: 'api-leverage',
    group: 'API',
    title: '改杠杆率',
    lead: '账户级 CTA 杠杆。不写交易所保证金，也不改 Exec Redis。',
    content: (
      <>
        <Endpoint method="PUT" path={`${MANAGER_ACCOUNT}/leverage`} />
        <CodeBlock label="curl">{`curl --noproxy '*' -sS -X PUT \\
  '${MANAGER_ACCOUNT}/leverage' \\
  -H 'Content-Type: application/json' \\
  -d '{"leverage": 2}'`}</CodeBlock>
        <p>
          请求体只接受大于 0 的 <code>leverage</code>。成功返回 studio 和最新{' '}
          <code>capacity</code>（含 <code>buying_power_usdt</code>、
          <code>bound_notional_usdt</code>、<code>remaining_notional_usdt</code>）。实时权益：
        </p>
        <Endpoint method="GET" path={`${MANAGER_ACCOUNT}/live`} />
      </>
    ),
  },
  {
    id: 'api-allocations',
    group: 'API',
    title: '改策略占比',
    lead: '一次提交本账户全部已启用策略的占比。页面「保存占比」走这条接口。',
    content: (
      <>
        <Endpoint method="PUT" path={`${MANAGER_ACCOUNT}/allocations`} />
        <CodeBlock label="curl">{`curl --noproxy '*' -sS -X PUT \\
  '${MANAGER_ACCOUNT}/allocations' \\
  -H 'Content-Type: application/json' \\
  -d '{"allocations":{"CTA_A":0.25,"CTA_B":0.75}}'`}</CodeBlock>
        <FieldTable
          rows={[
            { field: 'allocations', detail: '必须覆盖每一条绑定。每条大于 0，合计必须等于 1。键是 binding_name。' },
          ]}
        />
        <Callout>这只改 Manager 本地绑定。要让 Exec 仓位跟着变，保存后再点「发布到 Exec」。</Callout>
      </>
    ),
  },
  {
    id: 'api-shares',
    group: 'API',
    title: '改份数',
    lead: '给某条已启用策略单独设份数。浏览器主交互走占比接口，这条留给脚本。',
    content: (
      <>
        <Endpoint method="PUT" path={`${MANAGER_ACCOUNT}/bindings/CTA_NAME/shares`} />
        <CodeBlock label="curl">{`curl --noproxy '*' -sS -X PUT \\
  '${MANAGER_ACCOUNT}/bindings/CTA_SK_C40V6PosT1_LXY_filter_Position/shares' \\
  -H 'Content-Type: application/json' \\
  -d '{"shares": 3}'`}</CodeBlock>
        <p>
          <code>shares</code> 必须大于 0。启用策略时也可在 POST bindings 里带上，默认 1。
        </p>
      </>
    ),
  },
  {
    id: 'api-strategy',
    group: 'API',
    title: '全量推送',
    lead: '一次提交完整 config：8 个下单参数和 targets 都要有。',
    content: (
      <>
        <Endpoint method="POST" path={`${TRADE01_CONFIG}/api/strategy`} />
        <ul>
          <li>新策略：参数和 target 都会写入</li>
          <li>已有策略：只更新 target，参数保持 Redis 里的值</li>
        </ul>
        <CodeBlock label="JSON">{`{
  "strategy_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
  "config": {
    "single_order_usdt": 100.0,
    "orders_per_batch": 3,
    "maker_price_anchor": "own_best",
    "tick_spacing": 1,
    "batch_interval_ms": 500,
    "maker_timeout_ms": 1000,
    "max_maker_requotes": 2,
    "target_tolerance_usdt": 10.0,
    "targets": {"BTCUSDT": -0.006}
  }
}`}</CodeBlock>
      </>
    ),
  },
  {
    id: 'api-read',
    group: 'API',
    title: '查询与删除',
    lead: '这些路径都在 trade01 Config 基址下。',
    content: (
      <>
        <RouteTable
          rows={[
            { method: 'GET', label: '账户信息', path: <code>/api/bootstrap</code> },
            { method: 'GET', label: '策略名单', path: <code>/api/strategies</code> },
            { method: 'GET', label: '单个策略', path: <code>/api/strategy?name=...</code> },
            { method: 'DELETE', label: '移除策略', path: <code>/api/strategy?name=...</code> },
          ]}
        />
        <CodeBlock label="curl">{`curl --noproxy '*' '${TRADE01_CONFIG}/api/strategies'
curl --noproxy '*' '${TRADE01_CONFIG}/api/strategy?name=CTA_SK_C40V6PosT1_LXY_filter_Position'`}</CodeBlock>
      </>
    ),
  },
  {
    id: 'api-errors',
    group: 'API',
    title: '状态码',
    lead: '错误体形如 {"ok":false,"error":"..."}。',
    content: (
      <StatusTable
        rows={[
          { code: '200', meaning: '写入或查询成功', tone: 'ok' },
          { code: '202', meaning: '删除已受理', tone: 'ok' },
          { code: '400', meaning: '字段缺失、策略不存在、JSON 无效', tone: 'bad' },
          { code: '401', meaning: '改 order-parameters 时 token 不对', tone: 'bad' },
          { code: '404', meaning: '路径不存在，或请求未带账户前缀', tone: 'bad' },
          { code: '409', meaning: '参数乐观锁冲突，需要重新加载', tone: 'warn' },
          { code: '503', meaning: 'Exec Config 未配置写 token', tone: 'warn' },
        ]}
      />
    ),
  },
  {
    id: 'client',
    group: '客户端',
    title: 'exec_config_client',
    lead: '可从 Config 页下载，或使用 Exec 上的 scripts/exec_config_client.py。',
    content: (
      <>
        <CodeBlock label="常用命令">{`export EXEC_CONFIG_URL=${TRADE01_CONFIG}/

python3 exec_config_client.py get
python3 exec_config_client.py get CTA_SK_C40V6PosT1_LXY_filter_Position
python3 exec_config_client.py post-targets @targets.json
python3 exec_config_client.py post @strategy.json`}</CodeBlock>
        <Callout tone="warning">不要把 token 写进脚本或仓库。改参数请用 Manager 页面。</Callout>
      </>
    ),
  },
]

function chapterFromHash(hash: string): ChapterId {
  const id = hash.replace(/^#/, '') as ChapterId
  return chapters.some((chapter) => chapter.id === id) ? id : 'overview'
}

export function DocsPage() {
  const [activeId, setActiveId] = useState(() => chapterFromHash(window.location.hash))

  useEffect(() => {
    const onHashChange = () => setActiveId(chapterFromHash(window.location.hash))
    window.addEventListener('hashchange', onHashChange)
    return () => window.removeEventListener('hashchange', onHashChange)
  }, [])

  const activeIndex = chapters.findIndex((chapter) => chapter.id === activeId)
  const active = chapters[activeIndex] ?? chapters[0]
  const previous = activeIndex > 0 ? chapters[activeIndex - 1] : null
  const next =
    activeIndex >= 0 && activeIndex < chapters.length - 1 ? chapters[activeIndex + 1] : null
  const groups = useMemo(() => {
    const seen: string[] = []
    for (const chapter of chapters) {
      if (!seen.includes(chapter.group)) seen.push(chapter.group)
    }
    return seen.map((group) => ({
      group,
      items: chapters.filter((chapter) => chapter.group === group),
    }))
  }, [])

  function openChapter(id: ChapterId) {
    window.history.replaceState(null, '', `/manager/docs/#${id}`)
    setActiveId(id)
  }

  return (
    <AppShell active="docs" title="文档" subtitle="操作方法与 API" icon={BookOpen}>
      <div className="grid gap-6 xl:grid-cols-[260px_minmax(0,1fr)]">
        <aside className="h-fit overflow-hidden rounded-2xl border border-border bg-surface shadow-[var(--shadow-card)] xl:sticky xl:top-24">
          <div className="border-b border-border-soft px-5 py-4">
            <p className="text-[11px] font-semibold uppercase tracking-[0.16em] text-brand">目录</p>
            <p className="mt-1 text-sm font-semibold text-ink">CTA Manager</p>
          </div>
          <nav className="space-y-5 p-4">
            {groups.map((group) => (
              <section key={group.group}>
                <h2 className="mb-2 px-3 text-[11px] font-semibold uppercase tracking-[0.16em] text-subtle">
                  {group.group}
                </h2>
                <ul className="space-y-0.5">
                  {group.items.map((chapter) => (
                    <li key={chapter.id}>
                      <button
                        type="button"
                        className={cn(
                          'docs-sidebar-link',
                          chapter.id === active.id && 'is-active',
                        )}
                        onClick={() => openChapter(chapter.id)}
                      >
                        {chapter.title}
                      </button>
                    </li>
                  ))}
                </ul>
              </section>
            ))}
          </nav>
        </aside>

        <article className="overflow-hidden rounded-2xl border border-border bg-surface shadow-[var(--shadow-card)]">
          <header className="border-b border-border-soft bg-[linear-gradient(180deg,#f8fafc_0%,#ffffff_100%)] px-6 py-8 sm:px-10">
            <p className="text-[11px] font-semibold uppercase tracking-[0.16em] text-brand">
              {active.group}
            </p>
            <h2 id="gitbook-title" className="mt-2 text-3xl font-semibold tracking-tight text-ink">
              {active.title}
            </h2>
            <p className="mt-3 max-w-2xl text-sm leading-7 text-muted">{active.lead}</p>
          </header>
          <div className="docs-prose px-6 py-8 sm:px-10">{active.content}</div>
          <nav className="grid gap-3 border-t border-border-soft p-4 sm:grid-cols-2 sm:p-6">
            {previous ? (
              <button
                type="button"
                className="rounded-2xl border border-border px-4 py-4 text-left transition-colors hover:border-brand-ring hover:bg-brand-soft/40"
                onClick={() => openChapter(previous.id)}
              >
                <span className="flex items-center gap-1 text-xs text-subtle">
                  <ChevronLeft size={14} /> 上一章
                </span>
                <span className="mt-1 block text-sm font-semibold text-ink">{previous.title}</span>
              </button>
            ) : (
              <span />
            )}
            {next ? (
              <button
                type="button"
                className="rounded-2xl border border-border px-4 py-4 text-right transition-colors hover:border-brand-ring hover:bg-brand-soft/40"
                onClick={() => openChapter(next.id)}
              >
                <span className="flex items-center justify-end gap-1 text-xs text-subtle">
                  下一章 <ChevronRight size={14} />
                </span>
                <span className="mt-1 block text-sm font-semibold text-ink">{next.title}</span>
              </button>
            ) : null}
          </nav>
        </article>
      </div>
    </AppShell>
  )
}
