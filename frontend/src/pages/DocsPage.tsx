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

const EL01_GATEWAY = 'http://172.16.30.42:10041'
const JP_GATEWAY = 'http://13.115.227.29:4191'

const MANAGER_PATH = '/manager/api'
const CATALOG_PATH = `${MANAGER_PATH}/catalog`
const EXEC_PATH = '/exec_trade01/config/api'
const ACCOUNT_PATH = `${CATALOG_PATH}/accounts/binance_exec_trade01`

function currentGatewayOrigin() {
  if (typeof window === 'undefined') return EL01_GATEWAY
  const { protocol, hostname, port } = window.location
  if (!hostname) return EL01_GATEWAY
  const host = port ? `${hostname}:${port}` : hostname
  return `${protocol}//${host}`
}

function joinGateway(origin: string, path: string) {
  return `${origin.replace(/\/+$/, '')}${path.startsWith('/') ? path : `/${path}`}`
}

type ChapterId =
  | 'overview'
  | 'model'
  | 'bases'
  | 'catalog-position'
  | 'execution-cost'
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

function buildChapters(gateway: string): Chapter[] {
  const manager = joinGateway(gateway, MANAGER_PATH)
  const account = joinGateway(gateway, ACCOUNT_PATH)
  return [
  {
    id: 'overview',
    group: '概念',
    title: '概述',
    lead: '策略是全局目录；账户只负责启用、份数和发布。Exec 前缀只表示落到哪台交易机。',
    content: (
      <>
        <p>配置与发送分三层，不要混：</p>
        <SpecTable
          headers={['层', '归属', '做什么']}
          rows={[
            [
              '策略目录',
              '全局，不挂账户',
              '维护仓位策略（原始 targets）和下单策略（执行参数）',
            ],
            [
              '账户绑定',
              'el01 trade01-04 / 预留 trade05',
              '启用哪些策略并配置份数；仓位更新后按份数自动写入该账户 Exec',
            ],
            [
              'Exec 运行时',
              '该账户 Redis',
              '只读查询；仓位由 Manager 长连接直接写 Redis 并回读确认，再 iceoryx notify，30s 轮询兜底',
            ],
          ]}
        />
        <ApiTable
          rows={[
            {
              method: 'POST',
              path: `${CATALOG_PATH}/position-strategies`,
              summary: '创建/更新仓位策略，并自动推送到全部绑定账户',
            },
            {
              method: 'POST',
              path: `${CATALOG_PATH}/order-strategies`,
              summary: '创建/更新下单策略（全局）',
            },
            {
              method: 'POST',
              path: `${ACCOUNT_PATH}/bindings/{name}/publish`,
              summary: '手工重推一个已有绑定；日常仓位更新不必再调',
            },
            {
              method: 'GET',
              path: `${EXEC_PATH}/strategy?name=...`,
              summary: '只读查看该账户运行时；不能 POST 改 Redis',
            },
            {
              method: 'GET',
              path: `${CATALOG_PATH}/execution-cost`,
              summary: '按需对比每次仓位更新的实际费前成本与 1 分钟 mid TWAP 预估',
            },
          ]}
        />
        <Note tone="warn">
          el01 已部署 trade01–trade04，trade05 已配置但保持停机和禁用；jp-meta 仍仅启用 trade01。策略目录按主机独立。
        </Note>
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
              <code>strategy_name / targets / symbol_order_strategy_overrides</code>,
              '全局模板。targets 是原始目标数量；每个 symbol 可选择命名下单策略模板。',
            ],
            [
              '下单策略',
              <code>strategy_name + 9 个参数</code>,
              '全局模板，多账户可共用同一个 default_order。',
            ],
            [
              '账户',
              <code>source_id / alias</code>,
              'source_id 是稳定身份；alias 只改 Manager 展示名。交易所合约杠杆是独立保证金设置。',
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
            ['发布到 Exec 的 target', '仓位策略 qty × 该账户配置的份数'],
            ['signal', '原样发布，不随份数放大'],
          ]}
        />
        <Note>
          每个账户为每条绑定策略配置正数份数；发布数量 = 模板 qty × 份数。
        </Note>
      </>
    ),
  },
  {
    id: 'bases',
    group: '概念',
    title: '基址',
    lead: 'el01 和 jp-meta 是两台独立物理机、两套独立栈。浏览器与脚本的 GET/POST 都走该环境 Nginx，不要打 loopback 18201 / 18161。',
    content: (
      <>
        <SpecTable
          headers={['环境', 'Nginx 入口', '说明']}
          rows={[
            [
              'el01',
              <code>{EL01_GATEWAY}</code>,
              'el_dev 把 172.16.30.42:10041 转到 Exec 机 loopback Nginx :10051',
            ],
            [
              'jp-meta',
              <code>{JP_GATEWAY}</code>,
              '公网直达主机系统 Nginx :4191；根路径不是 CTA',
            ],
          ]}
        />
        <p className="mt-6 text-[13px] font-medium text-ink">el01 · {EL01_GATEWAY}</p>
        <SpecTable
          headers={['用途', 'Nginx 地址']}
          rows={[
            ['浏览器 / Manager API', <code>{joinGateway(EL01_GATEWAY, MANAGER_PATH)}</code>],
            ['策略目录', <code>{joinGateway(EL01_GATEWAY, CATALOG_PATH)}</code>],
            ['trade01 Exec Config', <code>{joinGateway(EL01_GATEWAY, EXEC_PATH)}</code>],
            [
              '文档页',
              <a href="/manager/docs/">{joinGateway(EL01_GATEWAY, '/manager/docs/')}</a>,
            ],
          ]}
        />
        <p className="mt-6 text-[13px] font-medium text-ink">jp-meta · {JP_GATEWAY}</p>
        <SpecTable
          headers={['用途', 'Nginx 地址']}
          rows={[
            ['浏览器 / Manager API', <code>{joinGateway(JP_GATEWAY, MANAGER_PATH)}</code>],
            ['策略目录', <code>{joinGateway(JP_GATEWAY, CATALOG_PATH)}</code>],
            ['trade01 Exec Config', <code>{joinGateway(JP_GATEWAY, EXEC_PATH)}</code>],
            [
              '文档页',
              <a href="/manager/docs/">{joinGateway(JP_GATEWAY, '/manager/docs/')}</a>,
            ],
          ]}
        />
        <Note>
          当前页若从某一侧打开，下面 curl 示例会用该页 origin。jp-meta 必须是{' '}
          <code>{JP_GATEWAY}</code>，不要写成 el01 的 <code>:10041</code>。
        </Note>
        <Note tone="warn">
          不要打 <code>/config/</code>。没有账户前缀时无法区分 trade01 / trade02 / trade03 / trade04 / trade05。
          也不要直接打 <code>127.0.0.1:18201</code> 或 <code>:18161</code>。
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
              path: `${CATALOG_PATH}/position-strategies`,
              summary: '列出全部仓位策略',
            },
            {
              method: 'POST',
              path: `${CATALOG_PATH}/position-strategies`,
              summary: '按 strategy_name upsert，并自动推送到全部绑定账户',
            },
            {
              method: 'DELETE',
              path: `${CATALOG_PATH}/position-strategies/{name}`,
              summary: '删除模板；已绑定账户需先停用',
            },
          ]}
        />
        <CodeBlock label="精简 POST body">{`{
  "strategy_name": "CTA_SK_C4V6PosT1_LXY_filter_Position",
  "targets": {
    "BTCUSDT": 0.004,
    "ETHUSDT": -0.016
  }
}`}</CodeBlock>
        <CodeBlock label="完整 POST body">{`{
  "strategy_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
  "targets": {
    "BTCUSDT": { "qty": -0.006, "signal": -1 },
    "ETHUSDT": { "qty": -0.54, "signal": 0 }
  },
  "symbol_order_strategy_overrides": {
    "BTCUSDT": "fast_order"
  }
}`}</CodeBlock>
        <FieldRows
          rows={[
            { field: 'strategy_name', detail: '必填，全局唯一；写入 Redis 时原样使用' },
            {
              field: 'targets',
              detail: '品种 → 数量。精简写法是裸数字；完整写法是 { qty, signal }',
            },
            { field: 'qty', detail: '目标数量，可为 0 或负数；自动推送时按该账户份数放大' },
            {
              field: 'signal',
              detail: '可省略，默认 0。只允许 -2/-1/0/1/2。±1 表示该品种本轮全部用 taker',
            },
          ]}
        />
        <Note>
          日常推仓位用精简版即可：只传策略名和裸数字 qty。省略的 signal 按 0。
        </Note>
      </>
    ),
  },
  {
    id: 'execution-cost',
    group: '策略目录',
    title: '执行成本',
    lead: '按需查询，不是实时任务。对比每次仓位更新窗口内的实际成交 VWAP 与 1 分钟 mid TWAP，都是费前。',
    content: (
      <>
        <Endpoint
          method="GET"
          path={`${CATALOG_PATH}/execution-cost`}
          summary="从归档仓位更新、5s mid 条和 Exec 成交即时生成报告"
        />
        <CodeBlock label="curl">{`curl --noproxy '*' -sS \\
  '${manager}/catalog/execution-cost?startMs=1755648000000&endMs=1755734400000&windowSec=300'`}</CodeBlock>
        <FieldRows
          rows={[
            { field: 'startMs / endMs', detail: '按仓位更新 received_at 过滤；省略 start 从最早开始，省略 end 到现在' },
            { field: 'windowSec', detail: '每次更新的最长执行窗口，默认 300（5 分钟），上限 86400' },
            { field: 'sourceIds', detail: '逗号分隔账户；省略则全部' },
            { field: 'strategyName', detail: '只看一个仓位策略；省略则全部' },
            {
              field: 'intended_qty',
              detail: '模板 qty × 当时归档的 shares − 快照 current_qty',
            },
            {
              field: 'twap_cost_before_fee_usdt',
              detail: 'intended × (窗口 TWAP − 到达 mid)。窗口从这次更新起按连续 1 分钟切桶；每桶对其中 5 秒 mid 等权平均，5 分钟就是 5 个 1 分钟 mid 再等权平均',
            },
            {
              field: 'actual_cost_before_fee_usdt',
              detail: '窗口内归属该策略的成交 filled × (VWAP − 到达分钟 mid)',
            },
          ]}
        />
        <Note>
          价格用 Manager 自己的 5 秒 mid 条。假设窗口内均匀执行：从这次 POST
          时刻起切连续 1 分钟，每分钟对其中 5 秒 mid 等权平均（满分钟 12 根），
          再对这几个 1 分钟 mid 平均。5 分钟窗口就是 5 个 1 分钟 mid。成交只认
          from_key_text 为 batch_exec:&lt;strategy_name&gt; 的 uniform_orders。
          窗口从这次 POST 开始，遇到同策略下一次更新提前结束。只统计归档里带有
          published_accounts（含当时 shares）的消息；份数以该条消息为准，不用当前目录回填。
        </Note>
        <Note tone="warn">
          这是查询生成，不写 Exec RocksDB，也不进交易热路径。浏览器在 /manager/execution-cost/。
        </Note>
      </>
    ),
  },
  {
    id: 'target-signal',
    group: '策略目录',
    title: 'qty 与 signal',
    lead: 'POST 可以用裸数字；写入 Redis 后每条仓位都是 {qty, signal}。signal 只影响该品种这一轮怎么成交。',
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
              path: `${CATALOG_PATH}/order-strategies`,
              summary: '列出全部下单策略',
            },
            {
              method: 'POST',
              path: `${CATALOG_PATH}/order-strategies`,
              summary: '按 strategy_name upsert',
            },
            {
              method: 'DELETE',
              path: `${CATALOG_PATH}/order-strategies/{name}`,
              summary: '删除模板',
            },
          ]}
        />
        <CodeBlock label="POST body">{`{
  "strategy_name": "default_order",
  "order_parameters": {
    "single_order_usdt": 100.0,
    "orders_per_batch": 3,
    "max_batch": 20,
    "maker_price_anchor": "own_best",
    "tick_spacing": 1,
    "batch_interval_ms": 500,
    "maker_timeout_ms": 1000,
    "max_maker_requotes": 2,
    "target_tolerance_usdt": 10.0
  }
}`}</CodeBlock>
        <Note>
          下单策略是可复用的完整模板。账户 binding 选择默认模板；仓位策略可以通过
          symbol_order_strategy_overrides 为特定 symbol 选择另一条命名模板。max_batch 限制一次
          目标更新的预估批次数；Exec 按目标激活时的 mark price 计算动态单笔金额，并取该值与
          single_order_usdt 的较大者。
        </Note>
      </>
    ),
  },
  {
    id: 'account-studio',
    group: '账户绑定',
    title: '账户与合约杠杆',
    lead: '账户 studio 保存策略绑定和份数；合约杠杆按单个 symbol 查/设交易所保证金杠杆。',
    content: (
      <>
        <ApiTable
          rows={[
            {
              method: 'GET',
              path: `${ACCOUNT_PATH}`,
              summary: '读取本账户的策略绑定、份数与 Maker/Taker 估算费率',
            },
            {
              method: 'PUT',
              path: `${ACCOUNT_PATH}/fee-rates`,
              summary: '更新 Maker/Taker 费率，接受任意有限小数，写入 PostgreSQL 并立即重算 NAV',
            },
            {
              method: 'GET',
              path: `${ACCOUNT_PATH}/contract-leverage?symbol=BTCUSDT`,
              summary: '查询该账户单个 symbol 的交易所当前合约杠杆；真相是交易所实时值',
            },
            {
              method: 'PUT',
              path: `${ACCOUNT_PATH}/contract-leverage`,
              summary: '按单个 symbol 调用交易所 setLeverage，不改 CTA 仓位，也不通知 pre-trade',
            },
          ]}
        />
        <CodeBlock label="curl">{`curl --noproxy '*' -sS \\
  '${account}'

curl --noproxy '*' -sS -X PUT \\
  '${account}/fee-rates' \\
  -H 'Content-Type: application/json' \\
  -d '{"maker_fee_rate":-0.00005,"taker_fee_rate":0.000146}'

curl --noproxy '*' -sS \\
  '${account}/contract-leverage?symbol=BTCUSDT'

curl --noproxy '*' -sS -X PUT \\
  '${account}/contract-leverage' \\
  -H 'Content-Type: application/json' \\
  -d '{"symbol":"BTCUSDT","contract_leverage":5}'`}</CodeBlock>
        <FieldRows
          rows={[
            {
              field: 'maker_fee_rate / taker_fee_rate',
              detail: '任意有限小数；负数表示返佣。保存后立即重算 Manager NAV，不改交易',
            },
            {
              field: 'contract_leverage',
              detail: '交易所当前保证金杠杆。GET 读实时值；PUT 设置 1–125',
            },
            {
              field: 'recorded_contract_leverage',
              detail: '本地上次 PUT 的对照，可能为空；不以它为准',
            },
          ]}
        />
        <Note>
          合约杠杆读该账户 Exec env.sh。Binance STANDARD 走 fapi，UNIFIED 走 papi；OKX 走 leverage-info / set-leverage。jp-meta 若没有 env.sh，查询会 502。
        </Note>
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
              path: `${ACCOUNT_PATH}/bindings`,
              summary: '启用；binding_name 通常等于仓位策略名',
            },
            {
              method: 'DELETE',
              path: `${ACCOUNT_PATH}/bindings/{name}`,
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
    title: '份数',
    lead: '每个账户为每条绑定策略填写一个正数 shares；发布 qty = 模板 qty × shares。',
    content: (
      <>
        <Endpoint
          method="PUT"
          path={`${ACCOUNT_PATH}/bindings/{name}/shares`}
          summary="设置该绑定的份数"
        />
        <CodeBlock label="body">{`{"shares": 2.5}`}</CodeBlock>
        <Note>保存份数只改 Manager 本地。下一次仓位 POST 会按新份数自动推 Redis；也可以点重推立即应用。</Note>
      </>
    ),
  },
  {
    id: 'account-publish',
    group: '账户绑定',
    title: '发布到 Exec',
    lead: '日常仓位更新会自动推到全部绑定账户。这条接口只用于手工重推一个已有绑定。',
    content: (
      <>
        <Endpoint method="POST" path={`${ACCOUNT_PATH}/bindings/{name}/publish`} />
        <p>
          每次 <code>POST /catalog/position-strategies</code> 成功后，Manager
          找出所有绑定了该策略的账户，用各自 <code>qty × shares</code>、默认下单模板和 symbol
          覆盖模板拼成 Exec 标准 JSON，<code>signal</code> 不随份数放大，再由 Manager 自己的 Redis
          长连接写入该账户 key。连接断了会自动重连；写入后回读确认，再发 iceoryx
          notify。notify 只带策略名和 <code>updated_at_us</code>，不带仓位。
          <code>exec-pre-trade</code> 收到后立刻再读 Redis；notify 断了仍有 30s 轮询兜底。
          手工 publish 做的是同一件事，只作用于一个绑定。
        </p>
        <Note tone="warn">还没有绑定账户时，仓位 POST 只写目录，不会写 Redis。</Note>
      </>
    ),
  },
  {
    id: 'exec-read',
    group: 'Exec 运行时',
    title: '只读查询',
    lead: 'Exec Config 页面和脚本只能读 Redis。仓位与完整策略写入走 Manager publish；pre-trade 以 Redis 为真源，iceoryx 只负责立刻唤醒 reload。',
    content: (
      <ApiTable
        rows={[
          { method: 'GET', path: `${EXEC_PATH}/bootstrap`, summary: '账户 / venue / key 前缀' },
          { method: 'GET', path: `${EXEC_PATH}/strategies`, summary: '策略名单' },
          { method: 'GET', path: `${EXEC_PATH}/strategy?name=...`, summary: '单个策略运行时 JSON' },
        ]}
      />
    ),
  },
  {
    id: 'exec-params',
    group: 'Exec 运行时',
    title: '内部写口',
    lead: '这些接口只给 Manager 用。浏览器和脚本不要直接打。',
    content: (
      <>
        <Endpoint
          method="POST"
          path={`${EXEC_PATH}/strategy`}
          summary="Manager 在本机 loopback 调用；浏览器和脚本不要打这条"
        />
        <Endpoint
          method="POST"
          path={`${EXEC_PATH}/order-parameters`}
          summary="只改已存在策略的 9 个参数；带 targets 会被拒绝"
        />
        <Endpoint
          method="DELETE"
          path={`${EXEC_PATH}/strategy?name=...`}
          summary="请求移除策略"
        />
        <Note>
          浏览器和外部脚本只写 Manager 的 catalog 接口；由 Manager 写 Redis 并通知
          exec-pre-trade。参数更新走 loopback Exec Config 的 order-parameters。
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
          ['400', '字段缺失、策略不存在、份数不是正数、合约杠杆缺 symbol'],
          ['404', '路径不存在或缺少账户前缀'],
          ['409', '参数乐观锁冲突'],
          ['502', '交易所/env.sh 不可用，或 Exec Config 不可达'],
        ]}
      />
    ),
  },
  {
    id: 'client',
    group: '附录',
    title: '客户端',
    lead: '仓位推送脚本从 Manager 下载，经 Nginx 写 catalog。',
    content: (
      <>
        <CodeBlock label="download">{`# el01
curl --noproxy '*' -fsS -o manager_publish_client.py ${joinGateway(EL01_GATEWAY, `${MANAGER_PATH}/manager_publish_client.py`)}
# jp-meta
curl --noproxy '*' -fsS -o manager_publish_client.py ${joinGateway(JP_GATEWAY, `${MANAGER_PATH}/manager_publish_client.py`)}`}</CodeBlock>
        <CodeBlock label="update">{`# el01 和 jp-meta 是两台独立物理机。必须显式选一边。
python3 manager_publish_client.py --target el01 put-position @cta.json
python3 manager_publish_client.py --target jp-meta put-position @cta.json
python3 manager_publish_client.py --target el01 get-contract-leverage binance_exec_trade01 BTCUSDT
python3 manager_publish_client.py --target jp-meta get-contract-leverage binance_exec_trade01 BTCUSDT
python3 manager_publish_client.py --target el01 set-contract-leverage binance_exec_trade01 BTCUSDT 5
python3 manager_publish_client.py --target jp-meta get-execution-cost --window-sec 300`}</CodeBlock>
        <CodeBlock label="cta.json 精简版">{`{
  "strategy_name": "CTA_SK_C4V6PosT1_LXY_filter_Position",
  "targets": {
    "BTCUSDT": 0.004,
    "ETHUSDT": -0.016
  }
}`}</CodeBlock>
        <CodeBlock label="cta.json 完整版">{`{
  "strategy_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
  "targets": {
    "BTCUSDT": { "qty": -0.006, "signal": -1 },
    "ETHUSDT": { "qty": -0.54, "signal": 0 }
  }
}`}</CodeBlock>
        <CodeBlock label="python">{`#!/usr/bin/env python3
from urllib.request import Request, urlopen
import json

# 当前页所在环境的 Nginx。jp-meta 必须是 ${JP_GATEWAY}
MANAGER = "${manager}"

payload = {
    "strategy_name": "CTA_SK_C4V6PosT1_LXY_filter_Position",
    "targets": {
        "BTCUSDT": 0.004,
        "ETHUSDT": -0.016,
    },
}

def request(method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    headers = {"Accept": "application/json"}
    if data is not None:
        headers["Content-Type"] = "application/json"
    req = Request(MANAGER + path, data=data, headers=headers, method=method)
    with urlopen(req, timeout=5) as resp:
        raw = resp.read()
        return json.loads(raw) if raw else {"ok": True, "http_status": resp.status}

print(json.dumps(request("POST", "catalog/position-strategies", payload), ensure_ascii=False, indent=2))
`}</CodeBlock>
        <Note>
          标准库即可，不必先下载脚本。日常用精简 payload：只传策略名和裸数字 qty。一次 POST 后 Manager 按各绑定账户份数放大 qty，省略的 signal 按 0 写入 Redis。需要 taker-only 时用完整对象写 signal=±1。
        </Note>
        <Note>
          脚本只 POST Manager catalog；Redis 与 iceoryx 通知由 Manager 完成。el01 和 jp-meta 互不影响，必须用 --target 选一边。
        </Note>
      </>
    ),
  },
  ]
}

const CHAPTER_IDS: ChapterId[] = [
  'overview',
  'model',
  'bases',
  'catalog-position',
  'execution-cost',
  'target-signal',
  'catalog-order',
  'account-studio',
  'account-bind',
  'account-alloc',
  'account-publish',
  'exec-read',
  'exec-params',
  'errors',
  'client',
]

function chapterFromHash(hash: string): ChapterId {
  const id = hash.replace(/^#/, '') as ChapterId
  return CHAPTER_IDS.includes(id) ? id : 'overview'
}

export function DocsPage() {
  const [activeId, setActiveId] = useState(() => chapterFromHash(window.location.hash))
  const gateway = currentGatewayOrigin()
  const chapters = useMemo(() => buildChapters(gateway), [gateway])

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
  }, [chapters])

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
