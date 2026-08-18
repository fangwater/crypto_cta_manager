import { BookOpen, ChevronLeft, ChevronRight } from 'lucide-react'
import { useEffect, useMemo, useState, type ReactNode } from 'react'
import { AppShell } from '../components/AppShell'
import {
  ApiTable,
  CodeBlock,
  Endpoint,
  Note,
  SpecTable,
} from '../components/docs/DocsPrimitives'
import { cn } from '../lib/cn'

const GATEWAY = 'http://172.16.30.42:10041'
const MANAGER = `${GATEWAY}/manager/api`
const CATALOG = `${MANAGER}/catalog`
const EXEC = `${GATEWAY}/exec_trade01/config/api`
const ACCOUNT = `${CATALOG}/accounts/binance_exec_trade01`

type ChapterId =
  | 'overview'
  | 'model'
  | 'bases'
  | 'catalog-position'
  | 'target-signal'
  | 'catalog-order'
  | 'account-studio'
  | 'account-bind'
  | 'account-alloc'
  | 'account-publish'
  | 'exec-read'
  | 'exec-params'
  | 'errors'
  | 'client'

interface Chapter {
  id: ChapterId
  group: string
  title: string
  lead: string
  content: ReactNode
}

function FieldRows({
  rows,
}: {
  rows: Array<{ field: string; detail: ReactNode }>
}) {
  return (
    <SpecTable
      headers={['字段', '说明']}
      rows={rows.map((row) => [
        <code key="f" className="font-mono text-[12px]">
          {row.field}
        </code>,
        row.detail,
      ])}
    />
  )
}

const chapters: Chapter[] = [
  {
    id: 'overview',
    group: '概念',
    title: '概述',
    lead: '策略是全局目录；账户只负责启用、占比和发布。Exec 前缀只表示落到哪台交易机。',
    content: (
      <>
        <p>配置与发送分三层，不要混：</p>
        <SpecTable
          headers={['层', '归属', '做什么']}
          rows={[
            [
              '策略目录',
              '全局，不挂账户',
              '维护仓位策略（targets + 参考权益）和下单策略（执行参数）',
            ],
            [
              '账户绑定',
              'trade01 / trade02',
              '启用哪些策略、杠杆、占比；点发布后写入该账户 Exec',
            ],
            [
              'Exec 运行时',
              '该账户 Redis',
              '只读查询；写入只能由 Manager publish 带 token 完成',
            ],
          ]}
        />
        <ApiTable
          rows={[
            {
              method: 'POST',
              path: `${CATALOG}/position-strategies`,
              summary: '创建/更新仓位策略（全局）',
            },
            {
              method: 'POST',
              path: `${CATALOG}/order-strategies`,
              summary: '创建/更新下单策略（全局）',
            },
            {
              method: 'POST',
              path: `${ACCOUNT}/bindings/{name}/publish`,
              summary: '算 qty、拼 JSON，带 token 写入该账户 Redis',
            },
            {
              method: 'GET',
              path: `${EXEC}/strategy?name=...`,
              summary: '只读查看该账户运行时；不能 POST 改 Redis',
            },
          ]}
        />
        <Note tone="warn">当前只部署了 trade01。trade02 会有独立 Exec 前缀，策略目录仍全局共用。</Note>
      </>
    ),
  },
  {
    id: 'model',
    group: '概念',
    title: '数据模型',
    lead: '仓位策略与下单策略独立存在；账户通过绑定把二者组合，再用份数放大 target。',
    content: (
      <>
        <SpecTable
          headers={['对象', '关键字段', '说明']}
          rows={[
            [
              '仓位策略',
              <code>strategy_name / equity_usdt / targets</code>,
              '全局模板。equity 是单份参考名义，默认 10000，可按策略改。',
            ],
            [
              '下单策略',
              <code>strategy_name + 8 个参数</code>,
              '全局模板，多账户可共用同一个 default_order。',
            ],
            [
              '账户',
              <code>leverage</code>,
              '只存杠杆。可用名义 = 实时权益 × 杠杆。',
            ],
            [
              '绑定',
              <code>position × order × shares</code>,
              '账户启用记录。Exec 策略名 = 仓位策略名。',
            ],
          ]}
        />
        <SpecTable
          headers={['量', '公式']}
          rows={[
            ['可用名义', '实时权益 × 杠杆率'],
            ['已配置名义', 'Σ(份数 × 该策略 equity_usdt)'],
            ['占比', '该策略已配置名义 / 已配置名义合计'],
            ['发布到 Exec 的 target', '仓位策略 qty × 份数；signal 原样带上'],
          ]}
        />
        <Note>
          容量按名义聚合。1×10000 + 1×20000 = 30000 USDT，不会按统一 10000 折成「3 份」。
        </Note>
      </>
    ),
  },
  {
    id: 'bases',
    group: '概念',
    title: '基址',
    lead: 'Manager 管目录与绑定；Exec Config 管交易机运行时。',
    content: (
      <>
        <SpecTable
          headers={['用途', '基址']}
          rows={[
            ['浏览器 / Manager API', <code>{MANAGER}</code>],
            ['策略目录', <code>{CATALOG}</code>],
            ['trade01 Exec Config', <code>{EXEC}</code>],
            ['文档页', <a href="/manager/docs/">{GATEWAY}/manager/docs/</a>],
          ]}
        />
        <Note tone="warn">
          不要打 <code>{GATEWAY}/config/</code>。没有账户前缀时无法区分 trade01 / trade02。
        </Note>
      </>
    ),
  },
  {
    id: 'catalog-position',
    group: '策略目录',
    title: '仓位策略',
    lead: '全局接口，路径里没有账户 ID。',
    content: (
      <>
        <ApiTable
          rows={[
            {
              method: 'GET',
              path: `${CATALOG}/position-strategies`,
              summary: '列出全部仓位策略',
            },
            {
              method: 'POST',
              path: `${CATALOG}/position-strategies`,
              summary: '按 strategy_name upsert',
            },
            {
              method: 'DELETE',
              path: `${CATALOG}/position-strategies/{name}`,
              summary: '删除模板；已绑定账户需先停用',
            },
          ]}
        />
        <CodeBlock label="POST body">{`{
  "strategy_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
  "equity_usdt": 10000,
  "targets": {
    "BTCUSDT": { "qty": -0.006, "signal": -1 },
    "ETHUSDT": { "qty": -0.54, "signal": 0 }
  }
}`}</CodeBlock>
        <FieldRows
          rows={[
            { field: 'strategy_name', detail: '全局唯一；发布到 Exec 时原样使用' },
            { field: 'equity_usdt', detail: '单份参考名义，必须 > 0' },
            {
              field: 'targets',
              detail: '品种 → { qty, signal }。旧格式裸数字仍可读，视为 signal=0',
            },
            { field: 'qty', detail: '目标数量，可为 0 或负数；发布时按份数放大' },
            {
              field: 'signal',
              detail: '整数，只允许 -2/-1/0/1/2；省略按 0。±1 表示该品种本轮全部用 taker',
            },
          ]}
        />
      </>
    ),
  },
  {
    id: 'target-signal',
    group: '策略目录',
    title: 'qty 与 signal',
    lead: '运行时每条仓位是对象，不再是裸数字。signal 只影响该品种这一轮怎么成交。',
    content: (
      <>
        <CodeBlock label="发布到 Redis 的 targets">{`{
  "BTCUSDT": { "qty": -0.018, "signal": -1 },
  "ETHUSDT": { "qty": -1.62, "signal": 0 }
}`}</CodeBlock>
        <SpecTable
          headers={['signal', 'Exec 行为']}
          rows={[
            ['0 或省略', '默认：taker 转 maker'],
            ['+1 / -1', '该品种本轮全部用 taker，不再转 maker'],
            ['+2 / -2', '预留整数；当前按默认路径处理，直到 Exec 定义用途'],
          ]}
        />
        <Note>
          ±1 只作用于这一次该 symbol 的执行。下一轮若仍要 taker，需要再次发布同样的 signal。份数只乘 qty。
        </Note>
      </>
    ),
  },
  {
    id: 'catalog-order',
    group: '策略目录',
    title: '下单策略',
    lead: '同样是全局目录。浏览器主路径走这里，不要直接改 Exec 参数页。',
    content: (
      <>
        <ApiTable
          rows={[
            {
              method: 'GET',
              path: `${CATALOG}/order-strategies`,
              summary: '列出全部下单策略',
            },
            {
              method: 'POST',
              path: `${CATALOG}/order-strategies`,
              summary: '按 strategy_name upsert',
            },
            {
              method: 'DELETE',
              path: `${CATALOG}/order-strategies/{name}`,
              summary: '删除模板',
            },
          ]}
        />
        <CodeBlock label="POST body">{`{
  "strategy_name": "default_order",
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
        <Note>只接受上述 8 个参数字段。改完后要对该账户已启用绑定再点「发布到 Exec」才会进交易进程。</Note>
      </>
    ),
  },
  {
    id: 'account-studio',
    group: '账户绑定',
    title: '账户视图与杠杆',
    lead: '账户层只有杠杆、实时权益和绑定列表。策略模板不在这里。',
    content: (
      <>
        <ApiTable
          rows={[
            {
              method: 'GET',
              path: `${ACCOUNT}`,
              summary: 'studio + capacity',
            },
            {
              method: 'GET',
              path: `${ACCOUNT}/live`,
              summary: '只取 capacity（含实时权益）',
            },
            {
              method: 'PUT',
              path: `${ACCOUNT}/leverage`,
              summary: '改 CTA 配置倍数',
            },
          ]}
        />
        <CodeBlock label="curl">{`curl --noproxy '*' -sS -X PUT \\
  '${ACCOUNT}/leverage' \\
  -H 'Content-Type: application/json' \\
  -d '{"leverage": 2}'`}</CodeBlock>
        <FieldRows
          rows={[
            { field: 'buying_power_usdt', detail: '实时权益 × 杠杆' },
            { field: 'bound_notional_usdt', detail: 'Σ(份数 × 策略 equity)' },
            { field: 'remaining_notional_usdt', detail: '可用名义 − 已配置名义' },
          ]}
        />
      </>
    ),
  },
  {
    id: 'account-bind',
    group: '账户绑定',
    title: '启用 / 停用',
    lead: '把全局仓位策略挂到本账户，并指定用哪套下单策略执行。',
    content: (
      <>
        <ApiTable
          rows={[
            {
              method: 'POST',
              path: `${ACCOUNT}/bindings`,
              summary: '启用；binding_name 通常等于仓位策略名',
            },
            {
              method: 'DELETE',
              path: `${ACCOUNT}/bindings/{name}`,
              summary: '停用本地绑定，不自动删 Exec',
            },
          ]}
        />
        <CodeBlock label="POST body">{`{
  "binding_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
  "position_strategy_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
  "order_strategy_name": "default_order",
  "shares": 1
}`}</CodeBlock>
      </>
    ),
  },
  {
    id: 'account-alloc',
    group: '账户绑定',
    title: '占比与份数',
    lead: '占比按已配置名义加权。各策略独立填写，合计必须等于 100%。',
    content: (
      <>
        <Endpoint
          method="PUT"
          path={`${ACCOUNT}/allocations`}
          summary="一次提交全部绑定的占比；保存后按总名义反推份数"
        />
        <CodeBlock label="curl">{`curl --noproxy '*' -sS -X PUT \\
  '${ACCOUNT}/allocations' \\
  -H 'Content-Type: application/json' \\
  -d '{"allocations":{"CTA_A":0.25,"CTA_B":0.75}}'`}</CodeBlock>
        <Endpoint
          method="PUT"
          path={`${ACCOUNT}/bindings/{name}/shares`}
          summary="脚本按份数精确调整；浏览器主交互走 allocations"
        />
        <Note>这两条只改 Manager 本地。要让 Exec 仓位变化，还要 publish。</Note>
      </>
    ),
  },
  {
    id: 'account-publish',
    group: '账户绑定',
    title: '发布到 Exec',
    lead: '把账户绑定物化成该账户 Exec 上的策略：参数来自下单策略，qty = 仓位策略 × 份数。',
    content: (
      <>
        <Endpoint method="POST" path={`${ACCOUNT}/bindings/{name}/publish`} />
        <p>
          Manager 用仓位策略 <code>qty × shares</code> 和下单策略参数拼成 Exec
          标准 JSON，<code>signal</code> 不随份数放大。再带写 token 调用该账户{' '}
          <code>POST /api/strategy</code>。新建和更新都走这一次完整写入。
        </p>
        <Note tone="warn">未 publish 的绑定只存在 Manager PostgreSQL，交易进程看不到。</Note>
      </>
    ),
  },
  {
    id: 'exec-read',
    group: 'Exec 运行时',
    title: '只读查询',
    lead: 'Exec Config 页面和脚本只能读 Redis。仓位与完整策略写入走 Manager publish。',
    content: (
      <ApiTable
        rows={[
          { method: 'GET', path: `${EXEC}/bootstrap`, summary: '账户 / venue / key 前缀' },
          { method: 'GET', path: `${EXEC}/strategies`, summary: '策略名单' },
          { method: 'GET', path: `${EXEC}/strategy?name=...`, summary: '单个策略运行时 JSON' },
        ]}
      />
    ),
  },
  {
    id: 'exec-params',
    group: 'Exec 运行时',
    title: '内部写口',
    lead: '这些接口只给 Manager 用，必须带写 token。浏览器和脚本不要直接打。',
    content: (
      <>
        <Endpoint
          method="POST"
          path={`${EXEC}/strategy`}
          summary="Manager publish：完整参数 + 已按份数放大的 targets"
        />
        <Endpoint
          method="POST"
          path={`${EXEC}/order-parameters`}
          summary="只改已存在策略的 8 个参数；带 targets 会被拒绝"
        />
        <Endpoint
          method="DELETE"
          path={`${EXEC}/strategy?name=...`}
          summary="请求移除策略；同样需要 token"
        />
        <Note tone="warn">
          <code>POST /api/targets</code> 已删除。无 token 或 token 错误返回 401；未配置 token 返回 503。
        </Note>
      </>
    ),
  },
  {
    id: 'errors',
    group: '附录',
    title: '状态码',
    lead: '错误体形如 {"ok":false,"error":"..."}。',
    content: (
      <SpecTable
        headers={['HTTP', '含义']}
        rows={[
          ['200', '成功'],
          ['202', '删除已受理'],
          ['400', '字段缺失、策略不存在、占比合计不为 1'],
          ['401', 'Exec Redis 写 token 错误或缺失'],
          ['404', '路径不存在、缺少账户前缀，或 /api/targets 已移除'],
          ['409', '参数乐观锁冲突'],
          ['503', 'Exec 未配置写 token'],
        ]}
      />
    ),
  },
  {
    id: 'client',
    group: '附录',
    title: '客户端',
    lead: '仓位推送脚本挂在 Manager。旧的 exec_config_client.py 已删除。',
    content: (
      <>
        <CodeBlock label="download">{`wget ${MANAGER}/manager_publish_client.py
# 或
curl --noproxy '*' -fsS -o manager_publish_client.py ${MANAGER}/manager_publish_client.py`}</CodeBlock>
        <CodeBlock label="update + publish">{`export MANAGER_API_URL=${MANAGER}/

python3 manager_publish_client.py put-position @cta.json
python3 manager_publish_client.py publish binance_exec_trade01 CTA_SK_C40V6PosT1_LXY_filter_Position`}</CodeBlock>
        <CodeBlock label="cta.json">{`{
  "strategy_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
  "equity_usdt": 10000,
  "targets": {
    "BTCUSDT": { "qty": -0.006, "signal": -1 },
    "ETHUSDT": { "qty": -0.54, "signal": 0 }
  }
}`}</CodeBlock>
        <Note tone="warn">
          不要再 POST Exec Config。旧的 exec_config_client.py 已删除；裸数字 qty 仍可读，会当成 signal=0。
        </Note>
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
    <AppShell
      active="docs"
      title="文档"
      subtitle="策略目录 · 账户绑定 · Exec 推送"
      icon={BookOpen}
      className="!max-w-none !px-0 !py-0"
    >
      <div className="min-h-[calc(100vh-3.75rem)] lg:grid lg:grid-cols-[220px_minmax(0,1fr)]">
        <aside className="border-b border-border bg-surface lg:sticky lg:top-[3.75rem] lg:h-[calc(100vh-3.75rem)] lg:overflow-y-auto lg:border-b-0 lg:border-r">
          <div className="px-4 py-4">
            <p className="text-[10px] font-semibold uppercase tracking-[0.18em] text-muted">API</p>
            <p className="mt-1 text-sm font-semibold text-ink">CTA Manager</p>
          </div>
          <nav className="space-y-5 px-2 pb-6">
            {groups.map((group) => (
              <section key={group.group}>
                <h2 className="px-3 text-[10px] font-semibold uppercase tracking-[0.16em] text-subtle">
                  {group.group}
                </h2>
                <ul className="mt-1.5 space-y-0.5">
                  {group.items.map((chapter) => (
                    <li key={chapter.id}>
                      <button
                        type="button"
                        className={cn(
                          'w-full rounded-md px-3 py-1.5 text-left text-[13px] transition-colors',
                          chapter.id === active.id
                            ? 'bg-ink text-white'
                            : 'text-muted hover:bg-canvas hover:text-ink',
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

        <div className="bg-[linear-gradient(180deg,#f7f8fa_0%,#ffffff_140px)]">
          <article className="mx-auto w-full max-w-3xl px-5 py-8 sm:px-8 lg:px-10 lg:py-10">
            <p className="text-[11px] font-medium tracking-[0.14em] text-muted">{active.group}</p>
            <h2 id="gitbook-title" className="mt-2 text-[1.75rem] font-semibold tracking-tight text-ink">
              {active.title}
            </h2>
            <p className="mt-3 text-[15px] leading-7 text-muted">{active.lead}</p>
            <div className="docs-prose mt-8 border-t border-border-soft pt-6">{active.content}</div>

            <nav className="mt-12 grid gap-3 border-t border-border-soft pt-6 sm:grid-cols-2">
              {previous ? (
                <button
                  type="button"
                  className="rounded-lg border border-border bg-surface px-4 py-3 text-left hover:bg-canvas"
                  onClick={() => openChapter(previous.id)}
                >
                  <span className="flex items-center gap-1 text-[11px] text-subtle">
                    <ChevronLeft size={13} /> 上一章
                  </span>
                  <span className="mt-1 block text-sm font-medium text-ink">{previous.title}</span>
                </button>
              ) : (
                <span />
              )}
              {next ? (
                <button
                  type="button"
                  className="rounded-lg border border-border bg-surface px-4 py-3 text-right hover:bg-canvas"
                  onClick={() => openChapter(next.id)}
                >
                  <span className="flex items-center justify-end gap-1 text-[11px] text-subtle">
                    下一章 <ChevronRight size={13} />
                  </span>
                  <span className="mt-1 block text-sm font-medium text-ink">{next.title}</span>
                </button>
              ) : null}
            </nav>
          </article>
        </div>
      </div>
    </AppShell>
  )
}
