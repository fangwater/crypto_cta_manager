# 国内期货理论执行参考与成本复盘

2026-11-14。国内入口新增每日参考数据、理论执行价格和真实成交成本对照。
所有计算在 Manager 内完成，不加载交易 SDK、不访问交易柜台、不发布目标。

行情来自独立的 `market_replay` 服务：原生分钟 gRPC（18312）提供实际到期合约的
OHLC、单边成交手数、已含乘数的人民币成交额及数据水位。Manager 通过固定 Git
revision 的 `cn_market_proto` 消费契约，没有相对路径或共享数据库依赖。
每日 JSON 继续维护历史有效的费率、完整交易日时段、独立信号及显式报单关联；
可省略 `bars` 并按需查询 gRPC，也可带人工复核的离线分钟数据。
账户费率可单独按历史时点查询；成本报告目前仍使用每日参考包中的费用和关联。
完整交易日历、国内净值和执行对接仍在推进，不补造实际成交或策略身份。

## 各类数据的用途

| 数据 | 在 Manager 成本复盘中的用途 |
| --- | --- |
| 行情接口／协议 | 从 market_replay 获取实际合约的分钟价格、成交手数和成交额，生成 TWAP／VWAP 理论参考，与事实成交比较；协议只是数据传输约定 |
| 合约乘数 | 把合约报价和整数手换算成人民币成交金额、价差成本；也用于从成交额／手数还原分钟 VWAP |
| 最小价格跳动 | 校验参考价格、识别单位或合约规则冲突，并可把价差表示成跳数 |
| 交易时段 | 按柜台交易日选取完整交易分钟，跳过午休／休市并正确处理夜盘；不能把未知缺口当作休市 |
| 账户手续费规则 | 分开仓、平仓、平今、平昨，按手或按金额估算理论费用；必须具有相应历史时点的覆盖 |
| 保证金规则 | 为后续保证金占用／杠杆分析提供依据，当前价格成本计算不依赖它 |

例如实际成交价比参考价高 2 元、买入 3 手、乘数 5，则价差成本为 `2 × 3 × 5 = 30` 元。
账户查询所得费率是估算规则，不是逐笔实际扣费。尤其 CTP 未报告事实手续费时，
可独立展示理论费用，不能把它回填成事实手续费或据此宣称费用差已经可核对。
读取已有订单／成交和同步落盘不依赖以上行情、时段或费率服务。

按操作者 2026-11-17 的选择，国内模式不区分 maker／taker，不按该分类展示费用或
拆分报表。手续费统一按合约及开仓、平仓、平今、平昨规则维护和计算；实际扣费与
估算费用仍分别标识。

## 持仓成本页面

`GET /api/catalog/acquisition-cost` 为 `/manager/acquisition-cost/` 提供账户范围内的成本复盘。
`sourceIds` 指定一个有查看权限的账户，`startMs` 和 `endMs` 为 Unix 毫秒且包含两端，支持最近
30 天。事实成交按持久化的接收时间选择，使用交易所原生交易日查找参考，夜盘保留其柜台交易日。
区间开始前发布的基准只有关联到区间内的事实成交时才进入比较。明细按 `rowSymbol`、`rowSort`
筛选和排序后分页，汇总保持完整查询区间的口径。

报表金额字段以 `_quote` 结尾并以人民币计量，数量为整数手。用完全一致的事实手数比较成交价格，
同量理论成交额用于归一化价格差和费后差。参考文件必须提供明确的信号与原生报单关联；没有关联的
成交仍计入事实指标。若合约乘数、已报告的人民币手续费或对应开平手续费规则缺失，相关合计为
`null`。策略归属和订单信号时间缺少可核验的参考关联时保留未知。

事实成交额优先使用原生报单合约记录或日参考包的乘数。二者均缺失时，已配置的行情服务可以提供
同一交易所、同一原生合约及同一交易日的合约乘数。Manager 先复用已验证的归档缓存或查询成交
接收时刻的一个分钟。分钟回报无法证明成交的原生交易日时，再查询该交易日的有成交分钟以校验
合约元数据。若服务缺少该原生期权的数据，成交额继续为空。该查询仅用于事实成交额。

每次浏览器查询限制为 200 万条扫描记录、25 万条区间成交、128 个参考文件及 128 MiB 参考读取量。
报单按完整原生键批量读取。分钟参考和乘数补齐各最多查询 32 个合约交易日，各有 12 秒行情预算。
同一合约交易日的已核验乘数在进程内保留 10 分钟，无可用乘数的查询结果保留 1 分钟。

## 原生 gRPC 行情契约

```toml
[market_data]
grpc_url = "http://127.0.0.1:18312"
cache_root = "/absolute/manager-owned/native-market-cache"
```

客户端调用 `cn_market.v1.CnFuturesMarket/GetMinuteBars`，显式发送交易所、原生合约、
柜台交易日和 `include_empty_minutes=true`。保留执行记录的大小写和月份编码，
仅在边界核对 provider 返回的 native/canonical code；不改为主连。
合约乘数、tick、交易日、分钟顺序及 `VWAP = turnover_cny / volume_lots / multiplier`
均校验。已有参考包的乘数或 tick 冲突时拒绝覆盖。

未归档分钟必须带 `provisional`，且 `open_time + 60000 <= DataState.live_through_ms`
才能进入计算，另要求本机已收盘。未追平的分钟保持缺失；零量且价格未知的分钟
也保持缺失。`mid_price` 是分钟末中间价，provider 的 `twap` 是分钟内时均价，
两者不代替 Manager 信号执行窗口的 TWAP／VWAP，也不用作到达价。

缓存位于独立的 `cache_root/<venue>/<symbol>/<trading_day>.json`。只有 provider
声明已归档、全部返回分钟均已完成的交易日才写持久缓存。盘中数据每次重新查询
provider，不按本机时间永久固化；归档缺口在下一次需要该分钟时重查。同进程同日
并发请求串行合并，已缓存的完整查询及重启后查询不重复下载。最多 4 个 gRPC 请求、
64 个排队查询，每个响应上限 8 MiB；端点直连，无环境代理或公开行情回退。

可用 SDK-free CLI 验证真实服务，无须 PostgreSQL 或执行数据库：

```bash
cn_futures_reference --config /absolute/manager.toml market \
  --venue SHFE --symbol sn2612 --trading-day 20261117
```

登录后也可按授权账户查询 `/manager/api/native/sources/<source_id>/market`，
参数为 `venue`、`symbol`、`trading_day`。响应明确给出水位、待完成分钟数和已定价分钟；
此接口不推断该账户实际持仓或策略归属。

## 账户费率服务

```toml
[reference_service]
api_url = "http://127.0.0.1:8765"
[reference_service.accounts]
sh_dev_chuangyuan01_ctp = "chuangyuan01"
```

映射左侧是配置中稳定的 CTP `source_id`，右侧是参考服务独立维护的账户别名，
不从账户标签或目录猜测。客户端只调用 `GET /api/v1/account-rates`，不连接柜台，
也不写参考服务或 Manager 数据库。请求明确指定原生交易所、合约和带时区的
`asOf`。响应的账户、合约、观察时间、回执哈希及记录哈希均检查；首条观察前
返回空，未来观察或身份冲突拒收。不借用同品种其他合约或其他账户的费率。

费用仍分开仓、平仓、平今、平昨。CTP 账户查询的 `close` 对应普通平仓费率，
平昨使用该费率，平今使用独立的 `closeToday`；不据此推断实际成交的开平类型。
该账户观察也返回多空保证金率及每手金额，但不是逐笔实际扣费或账户权益。

CLI `cn_futures_reference ... rates --source <id> --venue SHFE --symbol sn2612
--as-of 2026-11-17T04:00:00Z` 可独立验证。授权 HTTP 接口为
`/manager/api/native/sources/<id>/rates?venue=SHFE&symbol=sn2612&at_us=<Unix微秒>`，
返回 `fee_basis=account_rate_observation`，缺少历史观察时 `rates=null`。

## 每日参考文件

在国内 Manager 的配置顶层设置：

```toml
reference_root = "/absolute/manager-owned/references"
```

目录必须独立于所有执行数据库和 Manager secondary。每个启用账户、交易日使用
`<reference_root>/<source_id>/<YYYYMMDD>.json`。关闭账户不允许导入或读取。
文件最多 64 MiB，适合一个账户、一个交易日；不把多年行情装进一个文件。

从 [示例文件](../config/cn-futures-reference.example.json) 开始。该文件的行情、费率、
时段和关联**全部是格式演示数据**，不能用于真实交易评价。

| 字段 | 维护要求 |
| --- | --- |
| `source_id` / `trading_day` | 稳定账户标识及柜台交易日，必须与文件位置一致 |
| `provenance` | 数据来源、归一化口径及人工复核说明，不放凭据 |
| `contracts` | 实际到期合约，原生大小写；价格跳动、合约乘数及本账户当日费率 |
| `sessions` | 本交易日每个实际合约的全部交易区间，含夜盘、午休及节假日变化 |
| `bars` | 已收盘的一分钟 OHLC、单边增量成交手数和人民币增量成交额 |
| `signals` | 从独立证据保存的信号接收时间、方向、开平、整数手数及执行窗口 |
| `order_links` | 明确提供的 `(venue, session, order_ref)` 到参考编号的关联 |

时间必须带时区，如 `2026-11-12T21:00:00+08:00`。夜盘属于哪个交易日由文件
明确声明，程序不从自然日推算。`session` 保留完整组身份，所有原生编号使用
JSON 字符串。关联只用于人工参考比较，不建立或更改事实策略归属。
不按相同合约、相近时间、订单号末尾或本地提交标志猜测关联。

使用实际合约，不能把复权或主力连续合约的历史价格当作该交割合约的成交价。
`volume_lots` 和 `turnover` 必须是每分钟增量、同一单边口径；不能直接填入
累计 Tick 计数。若源数据采用双边统计，必须先一致地归一化。

## 离线维护

以下命令都不连接 PostgreSQL。`validate` 和 `import` 不打开执行数据库或发网络请求；
`report` 在启用行情配置后按需查询 market-replay gRPC。

```bash
cargo build --release --bin cn_futures_web --bin cn_futures_reference

# 只验证输入，不写任何文件。
cn_futures_reference validate --input /absolute/reference.json

# 原子安装到配置中的 reference_root。
cn_futures_reference --config /absolute/cn-manager.toml \
  import --input /absolute/reference.json

# 审核修订后明确替换；旧内容与新内容都保留在 history/<交易日>/<SHA256>.json。
cn_futures_reference --config /absolute/cn-manager.toml \
  import --input /absolute/reviewed-reference.json --replace

# 通过独立 RocksDB secondary 读完整交易日，输出成本报告。
cn_futures_reference --config /absolute/cn-manager.toml \
  report --source yida_sim01 --trading-day 20261113
```

同内容重复导入不产生新修订。不同内容默认拒绝，显式替换才生效。导入使用同目录
临时文件、文件同步和原子重命名，读者只看到完整旧文件或完整新文件。
崩溃留下的 `.lock` / `.pending` 是本地维护残留；确认没有导入进程后，保留或
检查该文件，再手工移走对应残留后重试。不要修改执行目录。

## 价格与费用

每个信号选择 `twap` 或 `vwap`，执行窗口为 1–60 个**交易分钟**。
从信号接收时刻向上对齐的首个完整分钟开始，正好整分钟则使用该分钟。
按文件明确声明的交易时段跨过休市，只能在同一柜台交易日内取完整窗口。
跨午休或夜盘的窗口采用交易分钟时钟，不能与 Crypto 的连续自然分钟直接等同。

```text
分钟成交均价 = turnover / (volume_lots × contract_multiplier)
TWAP = 窗口内每分钟成交均价的算术平均
VWAP = Σturnover / (Σvolume_lots × contract_multiplier)

价差成本 = 买卖符号 × 真实成交手数 × 合约乘数 × (实际成交价 - 理论成交价)
理论手续费 = 真实成交手数 × (per_lot + 理论成交价 × 合约乘数 × notional_rate)
手续费差 = 事实手续费 - 理论手续费
总成本差 = 价差成本 + 手续费差
```

买入符号为 +1，卖出为 -1，正成本表示实际执行更贵。
`open`、`close`、`close_today`、`close_yesterday` 分别维护，不把未明确区分的
`close` 自动当作平昨。费率支持按手、按金额或两者相加；负值可表示返还。
行情 OHLC 必须符合价格跳动，分钟成交均价必须落在最高／最低价之内，
以尽早发现手数、单边／双边或乘数错误。

执行窗口未收盘、分钟线缺失、零成交分钟、合约规则缺失或与委托原始合约规则
冲突时，该参考不可计算。缺失行情不以前一成交价或收盘价填充。
事实手续费未报告或币种尚未确认为 CNY 时保留未知。

## 浏览器与 API

国内页面的“成本分析”按一个账户和一个交易日查询：

```text
GET /api/native/sources/{source_id}/cost?trading_day=20261113
```

沿用账户查看权限及主机可见性边界。API 不返回本地文件路径、SDK 配置或原始
审计载荷。复盘扫描完整交易日，不受持仓／委托页面 `recent_rows` 的限制。

报告同时返回全部真实成交与每个信号的参考价格、分钟切片、权重和缺失原因。
`market_data_error` 标明行情接口或缓存故障，不隐藏已覆盖数据或事实成交。
`price_shortfall_cny`、`fee_shortfall_cny`、`total_shortfall_cny` 是完整覆盖才提供的
整体金额；缺少任意成交的对应覆盖时，相关整体金额为 null。
`covered_price_shortfall_cny` / `covered_fee_shortfall_cny` 是明确标识的已覆盖小计。
费用缺失不影响已具备价格证据的价差成本。零成交报告的成本金额保持 null。

这个报告衡量真实成交数量下的执行价格与手续费差，不衡量未成交机会成本、盘口
冲击、账户权益、保证金、结算盈亏或完整理论净值。后续净值需要独立的持仓锚点、
持仓估值行情和国内结算规则；它不由成本小计推导。

## 本地验证

2026-11-14：`cargo fmt --check`、`cargo check`（两个原生二进制）、
`cargo test --locked` 和两个原生二进制的 release 构建通过。21 项原生测试覆盖
乘数／方向／开平费用、夜盘和休市窗口、完整身份关联、缺失数据、原子修订、
原生单位、并发合并、缓存重启复用及缺口重试。
依赖隔离 PostgreSQL 或显式基准环境的既有测试仍按原设置跳过。
国内与 Crypto 前端均构建通过；lint 只有上游两个既有未使用声明警告。
release 命令的示例参考校验和配置校验通过，没有启动常驻服务或加载交易 SDK。
原生 gRPC 回归使用隔离的 loopback 服务，不执行交易 SDK。真实服务联调记录单独保留。
