import { BookOpen, ChevronLeft, ChevronRight } from 'lucide-react'
import { useEffect, useMemo, useState, type ReactNode } from 'react'
import { AppNav } from '../components/AppNav'

const GATEWAY = 'http://172.16.30.42:10041'
const TRADE01_CONFIG = `${GATEWAY}/exec_trade01/config`

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
  | 'api-strategy'
  | 'api-read'
  | 'api-errors'
  | 'client'

interface Chapter {
  id: ChapterId
  group: string
  title: string
  content: ReactNode
}

function CodeBlock({ children }: { children: string }) {
  const [copied, setCopied] = useState(false)

  return (
    <div className="gitbook-code">
      <button
        type="button"
        className="gitbook-copy"
        onClick={async () => {
          await navigator.clipboard.writeText(children)
          setCopied(true)
          window.setTimeout(() => setCopied(false), 1200)
        }}
      >
        {copied ? '已复制' : '复制'}
      </button>
      <pre>
        <code>{children}</code>
      </pre>
    </div>
  )
}

const chapters: Chapter[] = [
  {
    id: 'overview',
    group: '开始',
    title: '概述',
    content: (
      <>
        <p>
          这份 GitBook 说明 CTA 的策略组合和 Exec Config
          API。仓位策略、下单策略在 Manager 里独立配置，再绑定到账户；发布后才写入 Exec。
        </p>
        <ul>
          <li>
            浏览器配置组合：打开{' '}
            <a href="/manager/config/">/manager/config/</a>
          </li>
          <li>
            脚本直接推仓位：<code>POST /exec_trade01/config/api/targets</code>
          </li>
          <li>
            脚本同时写参数和仓位：<code>POST /exec_trade01/config/api/strategy</code>
          </li>
        </ul>
        <p>当前已部署账户只有 <code>trade01</code>。以后 <code>trade02</code> 会使用独立前缀，不能和 trade01 共用入口。</p>
      </>
    ),
  },
  {
    id: 'model',
    group: '开始',
    title: '账户与策略',
    content: (
      <>
        <p>两层身份不要混用：</p>
        <table>
          <thead>
            <tr>
              <th>维度</th>
              <th>例子</th>
              <th>放在哪里</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>账户</td>
              <td>
                <code>trade01</code> / <code>trade02</code>
              </td>
              <td>URL 前缀，例如 <code>/exec_trade01/</code></td>
            </tr>
            <tr>
              <td>策略</td>
              <td>
                <code>CTA_SK_C40V6PosT1_LXY_filter_Position</code>
              </td>
              <td>JSON 里的 <code>strategy_name</code></td>
            </tr>
          </tbody>
        </table>
        <p>
          每个策略自己有一份 <strong>target</strong>（目标仓位）和一份 Exec
          维护的实际仓位。同一个账户里，两个策略的 BTC target 可以不同，互不覆盖。
        </p>
        <p>
          Exec 运行时仍按「账户 + 策略名」落 Redis。Manager 这边把原来混在一起的策略拆成仓位策略和下单策略，组合后再发布成 Exec 策略名。
        </p>
      </>
    ),
  },
  {
    id: 'studio',
    group: '开始',
    title: '策略组合',
    content: (
      <>
        <p>
          Manager 配置页把策略拆成两份独立目录，不再挂在账户下面。账户只负责杠杆和绑定；权益金额是仓位策略的参考尺度，用来算组合比例。
        </p>
        <table>
          <thead>
            <tr>
              <th>对象</th>
              <th>独立配置什么</th>
              <th>默认值</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>仓位策略</td>
              <td>
                策略名、<code>equity_usdt</code>、<code>targets</code>
              </td>
              <td>权益 10000 USDT</td>
            </tr>
            <tr>
              <td>下单策略</td>
              <td>策略名和 8 个下单参数</td>
              <td>与现有 Exec 默认一致</td>
            </tr>
            <tr>
              <td>账户</td>
              <td>杠杆率</td>
              <td>杠杆 1</td>
            </tr>
            <tr>
              <td>绑定</td>
              <td>仓位策略 × 下单策略，发布名写入 Exec</td>
              <td>无</td>
            </tr>
          </tbody>
        </table>
        <p>
          账户权益是实时变动的，不在这里填写，也不用它卡容量。绑定后的分配比例 = 该仓位策略参考权益 / 账户已绑定参考权益合计。例如两份策略分别是 1 万和 3 万，比例就是 25% 和 75%。杠杆只表示账户愿意放大多少，不参与容量校验。同一份仓位或下单策略可以绑到多个账户。
        </p>
        <p>
          浏览器在 <a href="/manager/config/">/manager/config/</a> 完成创建、绑定和发布。发布时：
        </p>
        <ul>
          <li>Exec 上还没有这个发布名：走 <code>POST /api/strategy</code> 一次写入参数和 target</li>
          <li>已经存在：分别走 <code>POST /api/targets</code> 和带 token 的 <code>POST /api/order-parameters</code></li>
        </ul>
        <p>未点「发布到 Exec」之前，组合只存在 Manager 本地 PostgreSQL，不会改交易进程。</p>
      </>
    ),
  },
  {
    id: 'entry',
    group: '开始',
    title: '入口地址',
    content: (
      <>
        <p>
          对外入口是 <code>{GATEWAY}</code>。trade01 的 Config 基址：
        </p>
        <CodeBlock>{`${TRADE01_CONFIG}/`}</CodeBlock>
        <table>
          <thead>
            <tr>
              <th>用途</th>
              <th>路径</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>综合总览</td>
              <td>
                <a href="/">{GATEWAY}/</a>
              </td>
            </tr>
            <tr>
              <td>净值中心</td>
              <td>
                <a href="/manager/">{GATEWAY}/manager/</a>
              </td>
            </tr>
            <tr>
              <td>下单配置（浏览器）</td>
              <td>
                <a href="/manager/config/">{GATEWAY}/manager/config/</a>
              </td>
            </tr>
            <tr>
              <td>Exec Config API</td>
              <td>
                <code>{TRADE01_CONFIG}/api/...</code>
              </td>
            </tr>
            <tr>
              <td>Exec Viz</td>
              <td>
                <code>{GATEWAY}/exec_trade01/</code>
              </td>
            </tr>
          </tbody>
        </table>
        <p>
          脚本必须带账户前缀。不要使用 <code>{GATEWAY}/config/</code>，它无法区分 trade01 / trade02。
        </p>
      </>
    ),
  },
  {
    id: 'op-params',
    group: '操作方法',
    title: '改下单参数',
    content: (
      <>
        <p>
          下单参数现在属于独立的下单策略，不再先选账户再改某个 Exec 策略。Exec 的{' '}
          <code>/exec_trade01/config/</code> 页面保持只读。
        </p>
        <ol>
          <li>
            打开 <a href="/manager/config/">策略组合</a>
          </li>
          <li>在「仓位策略」里创建或编辑 target 和权益金额</li>
          <li>在「下单策略」里创建或编辑 8 个下单参数</li>
          <li>在「账户组合」里设置杠杆，再把仓位策略和下单策略绑成发布名，页面会显示分配比例</li>
          <li>点「发布到 Exec」。未发布的组合不会进入交易进程</li>
        </ol>
        <p>
          8 个下单参数仍是：<code>single_order_usdt</code>、<code>orders_per_batch</code>、
          <code>maker_price_anchor</code>、<code>tick_spacing</code>、
          <code>batch_interval_ms</code>、<code>maker_timeout_ms</code>、
          <code>max_maker_requotes</code>、<code>target_tolerance_usdt</code>。
        </p>
      </>
    ),
  },
  {
    id: 'op-targets',
    group: '操作方法',
    title: '推送目标仓位',
    content: (
      <>
        <p>
          推送脚本只打 <code>POST /api/targets</code>。策略必须已经存在，不会顺手创建算法。
          <code>targets</code> 是整表替换，不是按品种合并；没写到的品种会从该策略 target 里消失。
        </p>
        <CodeBlock>{`python3 exec_config_client.py \\
  --url ${TRADE01_CONFIG}/ \\
  post-targets @targets.json`}</CodeBlock>
        <p>
          <code>targets.json</code> 示例：
        </p>
        <CodeBlock>{`{
  "strategy_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
  "targets": {
    "BTCUSDT": -0.006,
    "ETHUSDT": -0.54
  }
}`}</CodeBlock>
        <p>
          同一账户下另一个策略要另发一次，换 <code>strategy_name</code>。trade02
          部署后把 <code>--url</code> 改成 <code>/exec_trade02/config/</code>。
        </p>
      </>
    ),
  },
  {
    id: 'op-full',
    group: '操作方法',
    title: '同时推送参数和仓位',
    content: (
      <>
        <p>
          <code>POST /api/strategy</code> 一次提交下单参数和 target。新建策略时两者都会写入；策略已存在时只更新
          target，下单参数保持 Redis 里已有的值，避免覆盖 Manager 改过的配置。
        </p>
        <p>只改仓位用 <code>/api/targets</code>，只改下单参数用 Manager 的 <code>/api/order-parameters</code>。</p>
        <CodeBlock>{`python3 exec_config_client.py \\
  --url ${TRADE01_CONFIG}/ \\
  post @strategy.json`}</CodeBlock>
      </>
    ),
  },
  {
    id: 'api-targets',
    group: 'API',
    title: 'POST /api/targets',
    content: (
      <>
        <p>改某个已有策略的目标仓位。不需要 token。</p>
        <p>
          URL：<code>{TRADE01_CONFIG}/api/targets</code>
        </p>
        <CodeBlock>{`curl --noproxy '*' -X POST ${TRADE01_CONFIG}/api/targets \\
  -H 'Content-Type: application/json' \\
  -d '{
    "strategy_name": "CTA_SK_C40V6PosT1_LXY_filter_Position",
    "targets": {"BTCUSDT": -0.006, "ETHUSDT": -0.54}
  }'`}</CodeBlock>
        <table>
          <thead>
            <tr>
              <th>字段</th>
              <th>说明</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>
                <code>strategy_name</code>
              </td>
              <td>已存在的策略名</td>
            </tr>
            <tr>
              <td>
                <code>targets</code>
              </td>
              <td>
                品种到数量的对象，数量可为 0 或负数。整表替换。
              </td>
            </tr>
          </tbody>
        </table>
        <p>
          成功返回 <code>strategy_name</code>、<code>targets</code>、
          <code>updated_at_us</code>。未知策略返回 400：
          <code>strategy is not active</code>。
        </p>
      </>
    ),
  },
  {
    id: 'api-params',
    group: 'API',
    title: 'POST /api/order-parameters',
    content: (
      <>
        <p>
          只改下单参数，不改仓位。必须带写权限 token，且策略必须已存在。浏览器请走
          Manager，不要直接打 Exec Config。
        </p>
        <p>
          URL：<code>{TRADE01_CONFIG}/api/order-parameters</code>
        </p>
        <p>请求头：<code>Authorization: Bearer &lt;token&gt;</code></p>
        <CodeBlock>{`{
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
        <p>
          只接受上述 8 个参数字段。带 <code>targets</code> 会被拒绝。版本不一致返回 409，需要重新 GET 后再写。
        </p>
      </>
    ),
  },
  {
    id: 'api-strategy',
    group: 'API',
    title: 'POST /api/strategy',
    content: (
      <>
        <p>一次提交完整 <code>config</code>：8 个下单参数和 <code>targets</code> 都要有。</p>
        <p>
          URL：<code>{TRADE01_CONFIG}/api/strategy</code>
        </p>
        <ul>
          <li>新策略：参数和 target 都会写入</li>
          <li>已有策略：只更新 target，参数保持 Redis 里的值</li>
        </ul>
        <CodeBlock>{`{
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
    content: (
      <>
        <table>
          <thead>
            <tr>
              <th>方法</th>
              <th>路径</th>
              <th>说明</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>GET</td>
              <td>
                <code>/api/bootstrap</code>
              </td>
              <td>账户、venue、key 前缀</td>
            </tr>
            <tr>
              <td>GET</td>
              <td>
                <code>/api/strategies</code>
              </td>
              <td>策略名单和待删除名单</td>
            </tr>
            <tr>
              <td>GET</td>
              <td>
                <code>/api/strategy?name=...</code>
              </td>
              <td>单个策略的参数和 target</td>
            </tr>
            <tr>
              <td>DELETE</td>
              <td>
                <code>/api/strategy?name=...</code>
              </td>
              <td>请求移除策略，不走 Manager</td>
            </tr>
          </tbody>
        </table>
        <CodeBlock>{`curl --noproxy '*' '${TRADE01_CONFIG}/api/strategies'
curl --noproxy '*' '${TRADE01_CONFIG}/api/strategy?name=CTA_SK_C40V6PosT1_LXY_filter_Position'`}</CodeBlock>
      </>
    ),
  },
  {
    id: 'api-errors',
    group: 'API',
    title: '状态码',
    content: (
      <>
        <table>
          <thead>
            <tr>
              <th>HTTP</th>
              <th>含义</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>200</td>
              <td>写入或查询成功</td>
            </tr>
            <tr>
              <td>202</td>
              <td>删除已受理</td>
            </tr>
            <tr>
              <td>400</td>
              <td>字段缺失、策略不存在、JSON 无效</td>
            </tr>
            <tr>
              <td>401</td>
              <td>
                改 <code>order-parameters</code> 时 token 不对
              </td>
            </tr>
            <tr>
              <td>404</td>
              <td>路径不存在，或请求未带账户前缀</td>
            </tr>
            <tr>
              <td>409</td>
              <td>参数乐观锁冲突，需要重新加载</td>
            </tr>
            <tr>
              <td>503</td>
              <td>Exec Config 未配置写 token</td>
            </tr>
          </tbody>
        </table>
        <p>错误体形如 <code>{'{"ok":false,"error":"..."}'}</code>。</p>
      </>
    ),
  },
  {
    id: 'client',
    group: '客户端',
    title: 'exec_config_client',
    content: (
      <>
        <p>
          客户端可从 Config 页下载，或使用 Exec 上的{' '}
          <code>scripts/exec_config_client.py</code>。默认基址已经是 trade01 的账户入口。
        </p>
        <CodeBlock>{`export EXEC_CONFIG_URL=${TRADE01_CONFIG}/

# 列出策略
python3 exec_config_client.py get

# 查看一个策略
python3 exec_config_client.py get CTA_SK_C40V6PosT1_LXY_filter_Position

# 只推仓位
python3 exec_config_client.py post-targets @targets.json

# 同时推送参数和仓位
python3 exec_config_client.py post @strategy.json`}</CodeBlock>
        <p>
          也可用 <code>--url</code> 覆盖。不要把 token 写进脚本或仓库；改参数请用 Manager 页面。
        </p>
      </>
    ),
  },
]

function chapterFromHash(hash: string): ChapterId {
  const id = hash.replace(/^#/, '') as ChapterId
  return chapters.some((chapter) => chapter.id === id) ? id : 'overview'
}

export function DocsPage() {
  const [activeId, setActiveId] = useState(() =>
    chapterFromHash(window.location.hash),
  )

  useEffect(() => {
    const onHashChange = () => setActiveId(chapterFromHash(window.location.hash))
    window.addEventListener('hashchange', onHashChange)
    return () => window.removeEventListener('hashchange', onHashChange)
  }, [])

  const activeIndex = chapters.findIndex((chapter) => chapter.id === activeId)
  const active = chapters[activeIndex] ?? chapters[0]
  const previous = activeIndex > 0 ? chapters[activeIndex - 1] : null
  const next =
    activeIndex >= 0 && activeIndex < chapters.length - 1
      ? chapters[activeIndex + 1]
      : null
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
    <div className="app-frame gitbook-frame">
      <header className="app-header">
        <div className="app-header__inner">
          <div className="brand">
            <span className="brand__mark" aria-hidden="true">
              <BookOpen size={19} strokeWidth={2.1} />
            </span>
            <div>
              <h1>CTA GitBook</h1>
              <p>操作方法与 API</p>
            </div>
          </div>
          <div className="header-actions">
            <AppNav active="docs" />
          </div>
        </div>
      </header>

      <div className="gitbook-layout">
        <aside className="gitbook-sidebar" aria-label="GitBook 目录">
          {groups.map((group) => (
            <section key={group.group}>
              <h2>{group.group}</h2>
              <ul>
                {group.items.map((chapter) => (
                  <li key={chapter.id}>
                    <button
                      type="button"
                      className={chapter.id === active.id ? 'is-active' : ''}
                      onClick={() => openChapter(chapter.id)}
                    >
                      {chapter.title}
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </aside>

        <article className="gitbook-page" aria-labelledby="gitbook-title">
          <p className="eyebrow">{active.group}</p>
          <h2 id="gitbook-title">{active.title}</h2>
          <div className="gitbook-body">{active.content}</div>
          <nav className="gitbook-pager" aria-label="章节翻页">
            {previous ? (
              <button type="button" onClick={() => openChapter(previous.id)}>
                <ChevronLeft size={16} />
                <span>
                  <small>上一章</small>
                  {previous.title}
                </span>
              </button>
            ) : (
              <span />
            )}
            {next ? (
              <button type="button" onClick={() => openChapter(next.id)}>
                <span>
                  <small>下一章</small>
                  {next.title}
                </span>
                <ChevronRight size={16} />
              </button>
            ) : (
              <span />
            )}
          </nav>
        </article>
      </div>
    </div>
  )
}
