# FR 账户首页聚合

CTA Manager 主动轮询 NAV Manager 的只读接口，将全部已启用的
`funding_rate` 账户加入首页。当前接入目标仅为 jp-meta；账户数量由 NAV
目录决定，不固定为 9 个。两个项目只通过 HTTP 通信，各自保留自己的
数据库、历史数据、计算和运行目录。

## 展示与口径

- 首页可筛选全部、CTA、FR，再按账户、交易所和上海日期筛选。
- FR 与 CTA 共用昨日权益、权益、今日盈利、收益率、多空持仓、多空杠杆、
  轧差、15 日回撤、初始保证金及占比、数据时间和查询时间这些列。
- FR 权益和初始保证金来自账户风险订阅或 Viz 中同一账户监控器的事实值，
  选择带原始时间戳且有效的较新记录；持仓名义金额来自 FR Viz
  的事实 `pre_trade_exposure`，独立累计现货和合约腿，跳过合计行。
  有非零但未定价的资产时，持仓、轧差和杠杆保持未知；不影响独立有效的事实权益。
- NAV 用其现有 FR 数量 FIFO、手续费、实际资金费和利息计算方式重放，
  以事实权益减去区间 PnL 变化倒算上海零点权益。零点成交归入新一天。
  FR 保留 NAV 现有成交价格估值，不改用 CTA 的 Binance 分钟估值。
- 15 日回撤使用未采样的重放变化；历史不足时为空。最近 30 个完整上海日
  可查询，历史风险数值没有事实观测时保持为空，不由权益反推。
- NAV 每 15 秒采集事实值，历史重放至多每 300 秒一次；HTTP 请求只读缓存。
  历史派生缓存重启后重建。未定价的费用或利息使对应历史计算不可用。
- 该权益重建不包含未接入 NAV 的充值、提现或其他账户流水；不会制造这些流水。
- 汇总包含当前有权限且筛选后的账户。缺失或过期的数据不会作为零计入，
  也不会显示成完整总额；CTA 数据和其他 FR 账户继续展示。

FR 名称进入 `/nav/strategies/<slug>`，NAV 详情继续使用 NAV 自己的登录和权限。
首页的 FR 查看权限由 CTA 权限管理页单独管理，默认仅管理员可见。
FR 只登记到 `cta_external_overview_accounts`，不进入 Exec 账户目录，不能
绑定策略、发布目标或修改交易设置。查看授权单独保存在
`cta_external_overview_viewers`。

## 接口与配置

NAV 提供 `GET /api/integrations/fr-account-overview`，可选 `date=YYYY-MM-DD`。
网关地址为 `/nav-api/integrations/fr-account-overview`。响应含
`accounts`、`dates`、`generated_at_ms`、`loading`；账户字段沿用首页的
snake_case 指标，以及 `collection_status`（`live`、`stale`、`unavailable`、
`historical`）。`source_id` 是 NAV 的稳定 slug，不导出密码、环境文件路径或
原始成交。CTA 将其命名为 `external:<provider_id>:<slug>`。

接口必须使用 `Authorization: Bearer ...`。NAV 从
`CRYPTO_NAV_OVERVIEW_TOKEN` 读取主机环境中的 32–256 字符 token；缺少 token
时接口不接受请求，也不启动该聚合后台。普通浏览器 session 不授予机器接口权限。
CTA 只从配置指定的环境变量读 token，不向浏览器发送 token。

CTA 配置见 `config/cta-manager.example.toml` 的 `[dashboard.nav_overview]`。
默认关闭，使用网关 URL，禁用 HTTP 环境代理和重定向。最新结果每 15 秒主动
轮询，历史日期按需后台拉取并缓存 300 秒。保留最后成功结果，失败后继续重试；
首页明确标记过期，不会将最新值冒充历史值。单次响应限制 1 MiB。

新数据库由当前 `migrations/schema.sql` 初始化。现有数据库必须先显式执行
`scripts/apply_external_overview_schema.sql`，正常启动不执行 DDL。主机 token
应保存在权限 `0600` 的独立环境文件，由两个 Manager 的服务读取，禁止提交。
