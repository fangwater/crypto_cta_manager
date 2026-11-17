# 国内期货理论执行参考与成本复盘

2026-11-14。国内入口新增每日参考数据、理论执行价格和真实成交成本对照。
所有计算在 Manager 内完成，不加载交易 SDK、不访问交易柜台、不发布目标。

行情来自操作员自建的 Binance Kline 兼容 HTTP 服务，已确认 `volume` 为成交手数、
`turnover` 为人民币成交额。下面的每日 JSON 维护合约规则、交易时段、信号参考
及显式报单关联；使用 HTTP 时可省略 `bars`，Manager 按需读取并缓存行情。
也可用人工准备的分钟数据进行离线核验。文件不会补造实际成交或策略身份。

2026-11-17 服务核对：`market_replay` 实际提供国内分钟行情 gRPC（18312）及合约、
时段、费率 HTTP（8765）；本文件下面描述的是 Manager 当前已有的 Binance 形状
HTTP 客户端，不表示上述原生服务已经接入。接入 gRPC 时还必须使用服务的数据水位
和 `provisional` 判断最终分钟，不能仅按本机时钟固化当前日行情。

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

## 自建 HTTP 接口契约

请求参数与 Binance `/fapi/v1/klines` 一致，URL 可以自行选择：

```text
GET <api_url>?symbol=SHFE%3Acu2612&interval=1m&startTime=<Unix毫秒>&endTime=<Unix毫秒>&limit=1000
```

`startTime`、`endTime` 为实际自然时间的 Unix 毫秒，含首尾。Manager 用分钟 open
作为起点，用最后一分钟的 `open + 59999 ms` 作为终点。当前只请求 `1m`，每页
最多 1000 根。返回 Binance 形状的二维 JSON 数组，每行至少包含以下位置：

| 索引 | 内容 | 类型／单位 |
| --- | --- | --- |
| 0 | open time | 整数 Unix 毫秒，整分钟对齐 |
| 1–4 | open / high / low / close | 十进制字符串或 JSON 数值，合约报价 |
| 5 | volume | 单边、该分钟增量成交手数，尚未乘合约乘数 |
| 6 | close time | 整数，必须等于 open time + 59999 |
| 7 | quote volume / turnover | 单边、该分钟增量人民币成交额，已包含合约乘数 |
| 8–11 | Binance 其他列 | 可以照常返回，当前成本计算不使用 |

所以分钟成交均价为 `row[7] / row[5] / contract_multiplier`。
数字类型与字符串类型均可用于价格和成交量，时间戳必须为 JSON 整数。
缺失分钟直接省略；不要用上一分钟、主连价格或未完成分钟填充。
无数据返回 `[]`，服务失败返回非 2xx 状态。没有夜盘／休市信息时，不能根据
K 线缺口自行认定休市；交易时段仍由每日参考文件明确维护。

配置示例：

```toml
[market_data]
api_url = "http://127.0.0.1:18311/fapi/v1/klines"
cache_root = "/absolute/manager-owned/market-cache"

[market_data.symbols]
"SHFE:cu2612" = "SHFE:cu2612"
"INE:sc2612" = "INE:sc2612"
```

映射右侧是服务实际接受的 `symbol`，也可明确设置为 `cu2612`。映射不能冲突，
Manager 始终用左侧的交易所与实际合约隔离行情，不自动转大写或换成主连。
未配置 `[market_data]` 时不发 HTTP。没有公开 Binance 或其他 API 回退；
HTTP 禁用环境代理和重定向，单请求超时 10 秒，共享 HTTP 并发上限 4。

缓存位于独立的 `cache_root/<venue>/<symbol>/<trading_day>.json`，每个合约／交易日
一个文件。只下载信号执行窗口内缺少的已收盘分钟；同进程并发缺口合并，多个
账户共享相同市场缓存，重启后可复用。缺失返回不写负缓存，之后查询继续补缺。
HTTP 或某一合约失败不阻止其他合约和事实成交返回。每页先验证时段、OHLC、
成交额与乘数，再原子更新缓存；已有分钟不会重复下载。

缓存绑定接口 URL 与 HTTP symbol 的指纹，换数据源需选新的缓存目录。
如果提供方修订了历史行情，先备份并移走对应 Manager 行情缓存文件，再重查。
这个动作只针对 Manager 的行情缓存，不涉及执行库或不可变参考修订。

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

以下命令都不连接 PostgreSQL。`validate` 和 `import` 不打开执行数据库或发 HTTP；
`report` 在启用行情配置后按需查询自建接口。

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
Binance 兼容请求、HTTP 数据单位、并发合并、缓存重启复用及缺口重试。
依赖隔离 PostgreSQL 或显式基准环境的既有测试仍按原设置跳过。
国内与 Crypto 前端均构建通过；lint 只有上游两个既有未使用声明警告。
release 命令的示例参考校验和配置校验通过，没有启动常驻服务或加载交易 SDK。
HTTP 验证使用本地测试服务，操作员的实际行情地址和数据尚未提供、尚未联调。
