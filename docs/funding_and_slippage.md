# 执行滑点与账户 funding

净值页的“净值差滑点”使用同一个起点和截止时间：

- 费前：`(理论费前 NAV − 实际费前 NAV) / 实际成交额 × 10,000`。
- 费后：`(理论费后 NAV − 实际费后 NAV) / 实际成交额 × 10,000`。

实际成交额是区间内成交的绝对报价币金额，买卖均累计。正值表示实际表现较差，负值表示实际表现较好；没有成交额时 bps 为不可用。理论窗口尚未结束时，重新计算实际 NAV 至理论曲线的最后一点，避免比较不同截止时间。返回的 `execution_slippage` 包括起止时间、成交额、费前/费后差值及 bps。理论历史未就绪、Kline 缺失或查询不支持时，不返回伪造的零滑点。

这是净值差口径：实际使用最新成交价估值，理论使用已完成分钟收盘价，且目标执行进度可能不同，因此包含仓位及估值差异。成本分析的同成交量滑点使用现有的 factual fill 数量、方向和理论 VWAP；`price_shortfall_bps` 和新增 `after_fee_shortfall_bps` 都以**可比理论成交额**为分母。费后差 = 价格差 + 实际估算费用 − 同量理论费用；Maker rebate 保持负号。总计、分组及目标行使用相同口径。

## 默认关闭与固定 IP

每个账户在 Manager「账户配置」和「策略启用」页独立开启，开关和拉取间隔保存到 PostgreSQL，默认关闭。支持原生 Binance USD-M 的 STANDARD / Portfolio Margin，以及显式授权的 RapidX Binance USD-M；COIN-M 和 OKX funding 尚未接入。身份始终是 `source_id`。

主机 TOML 的首个 `[[sources]]` 之前配置固定出口：

```toml
[funding]
enable_token_env = "CTA_FUNDING_ENABLE_TOKEN"
# 实际分配给 Manager 的固定专用地址；不得与任何交易出口相同。
local_ip = "154.197.32.9"
public_ip = "154.197.32.9"
# 必须列出该主机所有运行中的 trade_engine TOML，而不只是此账户。
trade_engine_configs = [
  "/home/el01/binance_exec_trade01/trade_engine.toml",
  # ...其余运行中的账户...
]
# 有私网 NAT / 0.0.0.0 / :: 交易绑定时，必须显式列出其公网出口。
forbidden_public_ips = ["154.197.32.6"]
```

以上 IP 是 el01 配置示例，jp-meta 等主机必须改为自身已配置的专用地址。账户凭据仍只读取本账户 `env.sh`，不复制或记录密钥。HTTP 使用 `local_address` 固定绑定，禁用环境代理；不能绑定、实际公网地址与 `public_ip` 不符或出口与任意交易地址冲突时，停止 REST 请求，无默认路由或交易 IP 回退。每次采集都会重新校验交易 IP 配置。`0.0.0.0`/`::` 交易地址以不发包的方式解析实际本地地址；私网 NAT 的交易公网地址必须排除。el01 只允许保留的 `.9` / `.10` 专用地址。按本次操作要求，funding 账单查询使用与 trading 不一致的专用固定 IP。

## 明确的数据库维护

新数据库通过原有 `cta_web --init-db` 使用 `migrations/schema.sql`。现有数据库由运维明确执行 [scripts/apply_funding_schema.sql](../scripts/apply_funding_schema.sql)，新增 `cta_funding_accounts`、`cta_funding_symbols`、`cta_funding_income` 三张表和一个索引；整个操作是一个事务。不要在已含这些表的数据库重复执行。正常服务启动不会运行 DDL，也不使用 sqlx migration 历史。

已有 funding 账单表的数据库另行执行 [scripts/apply_funding_settings_schema.sql](../scripts/apply_funding_settings_schema.sql)，仅为账户目录新增默认关闭的开关、拉取间隔及最近通过测试的出口记录；账单不会删除。新数据库的当前 schema 已包含这些字段。

在主机私有环境文件设置 `CTA_FUNDING_ENABLE_TOKEN`（与 TOML 名称一致），不把明文写入仓库、数据库、日志或浏览器存储。在账户的 Funding 记录面板输入启用 token、选择拉取间隔（60–86400 秒，默认 300），点击「测试出口并开启」。API 首先验证 token，再固定绑定出口测试实际公网地址，并使用该账户凭据测试签名 funding income REST；全部通过后才保存开关及测试记录。任何测试失败保持关闭。设置开关或间隔立即生效，不需要重启服务；每次重新开启再次验证 token 和网络。关闭保留历史账单用于核对，取消待发请求，净值页总体盈亏切换到估算 Funding。只有该账户配置权限或管理员可修改；账户卡片及净值页显示开启、关闭、补齐或异常标志。其他账户保持关闭。

## 增量采集与盈亏

从本机该账号的完整只读订单历史发现 USD-M symbol，账户观测起点取最早订单创建时间，各 symbol 从自己的最早订单创建时间开始。历史记录没有创建时间时使用接收时间，再退到更新时间；未成交订单也参与币种及起点发现。起点不依赖资金费功能启用时间、当前净值窗口或 PostgreSQL 仓位快照；没有订单时等待，不使用当前时间创建起点。

已发现的 symbol 即使平仓、下架仍保留。如果发现更早订单，账户和受影响 symbol 的起点只向前扩展历史，受影响的 cursor 重新从该起点补拉，既有账单保留且按 `(source_id, venue, tran_id)` 去重。每次最多推进一个七日区间，分页完成后将原始十进制收支和 cursor 同事务保存，并立即刷新浏览器可见的收支及进度。历史未补齐时持续推进下一增量，仍受固定 IP、共享预算和 Retry-After 限制；补齐后按账户配置的拉取间隔轮询。已接近当前时间的 cursor 重读最近 24 小时以覆盖延迟入账。各账户单独运行，一处失败不终止其他账户。全部 funding worker 共用每滚动 60 秒最多 120 weight（四次 income 请求）预算及 Retry-After 冷却。

实际收支取 `incomeType=FUNDING_FEE`，正数收入、负数支出；不通过 funding rate × 持仓估算。Binance 仅提供最近三个月的 income，代码保守拒绝超出 89 天的未采集 cursor，不把已不可查询的旧区间标记完整。已有账单永久保存在 Manager PostgreSQL。持久化起点不可向后缩短历史；新增更早订单会安全扩展历史。超过 REST 保留范围的旧账单需要另行导入，不能伪造为完整覆盖。

“账户综合 PnL”展示独立 Funding、费前总体盈亏及费后总体盈亏：开启采集时使用 `订单 NAV + 该区间实际 Funding`，关闭时使用 `订单 NAV + 该区间估算 Funding`。已开启采集的账户进入净值页时默认使用账户综合口径，收益曲线默认勾选橙色“Funding（累计资金费）”及费后总体净值；Funding 曲线按阶梯显示区间累计收支，可单独取消勾选。顶部同时显示区间 Funding 金额，估算明确标注。关闭采集的账户默认使用策略归因；用户可手动切换两种口径。界面展示资金费汇总、状态和收益曲线。区间开始点为零，恰好在起点发生的 Funding 在后续总计保留；零时长查询保留边界收入。时间和币种过滤与账户 NAV 一致。状态、起点、检查进度和错误都通过 API 返回；采集开关开启但补齐中或采集异常时仍使用已存账单，并提示总体数值尚未完整。

实际 Funding 只归账户，不猜测策略分摊，不改变策略 FIFO、实现盈亏、交易费用或执行滑点。只有 USDT/USDC 且匹配对应 symbol 报价资产的账单可计入；API 同时保留分资产收支。存款、转账、利息、清算罚金等其他余额变动仍未纳入。账户 JSON 和 Arrow 均有独立 funding / 总体净值字段，保留订单净值字段原来的含义。实际账单字段在暂停后仍保留事实记录，浏览器根据采集开关选用实际或独立的估算字段；不将估算金额写入实际账单字段。

REST 端点和分页参考 Binance 官方 [USD-M income](https://developers.binance.com/docs/derivatives/usds-margined-futures/account/rest-api/Get-Income-History) 与 [Portfolio Margin UM income](https://developers.binance.com/docs/derivatives/portfolio-margin/account/Get-UM-Income-History)；实现参考独立 `crypto_nav_manager` 的 funding income 查询，不使用跨仓库路径或共享运行状态。

2026-10-15 操作员特别授权 jp-meta 的 lgq 三个 CTA（trade05/RX02、trade06/RX01、trade07/RX03）使用各自固定交易 IP 直接查询 RapidX funding。通过 `[funding].rapidx_trading_ip_sources` 按 source ID 列出例外；不匹配的 RapidX 账户拒绝采集，原生账户不能使用例外。`funding.local_ip` 必须与账户当前首个交易 IP 完全一致，HTTP 显式绑定且禁用代理和重定向，每轮验证配置的 `public_ip`；交易绑定变更即停止采集。本次三个账户均为 `172.31.35.228`，验证公网地址 `13.115.227.29`。其他原生账户继续要求独立非交易出口。

RapidX 使用官方 [Query Statement](https://apidocliquidity.readme.io/reference/query-statement) 的 `statementType=FUNDING_FEE` 和对应 `BINANCE_PERP_<BASE>_<QUOTE>`，只接受本 portfolio、BINANCE、PERP、匹配报价币与时间范围的完整分页。保留 `deltaAmount` 的原始有符号十进制，以 `statementId` 去重；不将利息、转账或清算费用当作 funding。最早本地订单及每个 symbol 的起点、七日增量、最近 24 小时重读、账单与 cursor 同事务提交均沿用原规则。RapidX 三账户共享每滚动分钟最多 40 次 funding 请求，低于接口 100 次上限并给 Exec 自身轮询留出余量；原生 income 预算仍为 4 次 / 120 weight。HTTP 限流冷却同时约束两个预算。

## 公开费率估算与策略归因（2026-10-15）

新增独立的 `[funding_rates]`，默认启用，不依赖账户 funding 开关或交易所 API
凭据。按操作员本次指令，公开 funding rate 可使用普通默认路由，也可用可选的
`local_ip` 固定出口；禁用 HTTP 环境代理。不改变账户查询、实际账单采集、Kline
各自既有的出口规则。

每个币种第一次请求直接以 `limit=1000` 查询最近 30 天，每小时结算的 720 条通常
一页即可。默认在后台预热 BTCUSDT、ETHUSDT、BNBUSDT、SOLUSDT、XRPUSDT、DOGEUSDT。
其他币种由可见账户的净值查询按需发现，支持 Binance USDT/USDC 永续及 Unicode
币种；现金、币本位与 OKX 不使用 Binance USD-M 费率。跨账户共用一个内存缓存，
同币种并发查询共用一次加载；进程重启重新加载，不创建数据库表或 RocksDB CF。
缓存每 1800 秒增量刷新并重读最近一小时，保留最近 30 天及一分钟查询边界余量；
30 天未查询的非默认币种会释放。并发上限 8，滚动每 300 秒至多 450 次请求，所有
请求共用 Retry-After 冷却。完整分页校验成功后才替换缓存；失败保留旧缓存，API
明确提示不可用。查询不等待历史 HTTP，加载状态驱动浏览器十秒后重试。

每次结算：`估算收入 = -结算整点前有符号基础币数量 × 该次公开 markPrice × fundingRate`。
常规结算将公开时间戳归到对应整点，忽略发布的毫秒偏移；只计算真实返回的结算记录，
不假定每个币每小时都收费。特殊结算保留其公布时间。缓存仍保存原始公开时间戳。
使用 NAV 同一代、同一快照选择、已校验且补齐的实际成交以及小时检查点，保留账户、
symbol、venue 隔离；策略内部对敲改变策略仓位但不改变账户总仓位。账户快照没有
策略身份的部分继续归 `__initial_position__`；策略分配快照下系统平仓的影响归
`__unallocated__`。成交与结算时间完全相同时按结算前仓位处理；API 保留结算前后
15 秒内的成交数量供诊断。[Binance 官方说明](https://www.binance.com/en/support/faq/detail/360033525031)
实际扣收可与名义时刻有 15 秒偏差，例如 08:00:05 开仓仍可能计费。按操作员要求，
该偏差仅记入文档，页面不显示说明或边界成交提示。估算依然使用整点前仓位，不以
账单反推或调整策略结算截点。恰好落在
仓位快照时刻的结算无法还原快照前仓位，返回不可用。USDT/USDC 分资产汇总。

`/api/timeline` 与 `/api/account-timeline` 增加 `estimated_funding`，提供账户估算、
各策略估算、费前/费后含估算资金费的曲线与加载/错误状态。JSON 原有订单 NAV、
交易费和执行滑点不变；Arrow 增加可空的 `estimated_funding_income_quote`、
`estimated_total_nav_before_fee_quote`、`estimated_total_nav_after_fee_quote`。
未完成或不可用时这些列为 null，不能把缺失数据当成零。正常区间起点仍为零，
边界发生的 funding 保留到后续点；零时长区间保留边界结算。

页面的策略 PnL 默认包含估算 Funding，策略表前两项数值分别是独立估算 Funding
和含估算 Funding 的净值，另外保留订单净值。新增「策略 Funding」曲线视图，
逐策略展示累计估算收支，共用策略筛选并显示零参考线。资金费最多显示八位小数，
避免小额收支显示成 0.00；独立 Funding 曲线不受费前/费后切换影响，缺失时不以
订单 PnL 或零金额代替。估算曲线、估算 Funding 默认勾选。
账户已启用采集时，默认进入账户口径，总体曲线使用实际账单；关闭采集后总体曲线
使用估算，即使保留历史账单也不例外。RapidX 使用相同开关规则。切到分策略时始终
使用整点估算，不分摊实际账单。估算与实际不重复相加；估算未就绪时明确显示加载或
不可用，不用历史实际账单代替。估算只支持当前时钟之前最近 30 天，旧区间
不触发 HTTP，也不伪造完整资金费。

已有账单用于验证。按相同账户、币种、币种资产和最接近的公开结算时间对齐（最大
允许 15 分钟入账偏移），仅核对各 symbol 已观测的范围。没有完整账单覆盖的时段
单列为未验证，不当作零。没有账单的最新结算需等待 15 分钟且采集 cursor 已经过
该时刻，才可认为账单确实缺失。误差阈值为 `max(1e-7, abs(实际金额)*1e-6)`。

只读工具 `nav_funding_check --config ... --source ... --days 30 --output ...` 可用
生产 PostgreSQL 快照、Exec 只读订单及已保存账单重复核对。它不会调用签名账户
接口，也不会变更采集开关或数据库。此次验证见 [核对结果](funding_estimate_20261015.md)。
核对确认 RapidX 有不同的结算仓位截点及标记价格取样；结算后的成交有的被计入，
有的没有，因此不能统一后移 15 秒。公开字段不足以逐笔复现其内部结算，保留实际
账单采集和历史账单。
