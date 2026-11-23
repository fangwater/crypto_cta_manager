# 国内期货 CTA 管理与观察

最后更新：2026-11-20 UTC。

`cn_futures_cta_manager` 从 `crypto_cta_manager` 的 `757ea71` 建立共同历史的本地 fork，分支为 `cn-futures`，`upstream` 指向原 GitHub 仓库。未改包名和既有 Crypto API；原生入口和读取器集中在 `src/native_futures/` 与 `src/bin/cn_futures_web.rs`。CN 使用 Crypto 的前端入口、页面与交互，资产字段按国内合约、手数和 CNY 展示。

本阶段支持易达、CTP 的账户目录、委托、事实成交、策略目录和授权，读取范围包括 SHFE、INE、DCE、CZCE、CFFEX、GFEX 六所。使用原 Manager 的登录、密码／会话实现、账户授权和私有策略目录表。国内 Manager 必须使用独立 PostgreSQL 数据库、凭据变量与 secondary 目录。Cookie 名为 `cn_cta_session`，与 Crypto 的 `cta_session` 分离。

2026-11-17 默认部署方向调整为本机一个 Manager 管理多个云端实盘，云端独立
`sync-server serve` 只读导出，本机独立 `sync-server pull` 增量写入专用副本；见
[国内期货增量同步](cn_futures_sync.md)。TOML 显式配置 `mode = "cn_futures"`。
原直接读取方式继续可用，Crypto 旧配置默认 `crypto`，不自动启动同步进程。

已接入 market-replay 原生分钟 gRPC，支持本地每日参考数据维护、分钟 TWAP／VWAP 和真实成交成本对照，
见[行情参考与成本复盘](native_reference_cost.md)。参考文件与执行数据库隔离；
成本对照不建立策略事实归属，也不代表已实现国内净值或结算盈亏。

观察进程不加载 SDK、不访问交易柜台，不发目标、不报撤单。策略目标单位为**整数手**，键为 `SHFE:cu2612`、`INE:sc2612P640` 等，保留原生大小写。正数为多仓、负数为空仓。保存只更新目录；没有执行发布或自动下单路径。未接入策略事实身份前，不能因为订单曾本地提交或合约与策略目标相同而推定成交归属。

国内多策略批量目标与归因见 [批量目标与归因](native_netting.md)。与 crypto 一致，策略账本由 Exec 持有；Manager 路由完整策略目标，只读取 Exec 的统一订单与成交记录计算净值。

## 配置与构建

复制 `config/cn-futures-manager.example.toml`，配置每个账户／执行部署唯一的 `id` 和执行进程 `flow/persist_manager` 的绝对路径。`secondary_root` 与所有执行数据库路径必须互不包含，校验会解析已有路径和父目录的符号链接。关闭的数据源不授予读取权限。可用 `source_viewers` 对管理员同样施加主机级可见性边界。

每个 source 必须显式配置 `adapter = "yida"` 或 `adapter = "ctp"`；同一 Manager 可混合读取，
同步导出端与接收端应使用相同配置。两种柜台写入相同的两个列族，Manager 不加载 SDK。
会话保留 `ctp:<front_id>:<session_id>`，不截取 OrderRef 或合并不同会话；未知手续费和
币种保持空值。合约保留原生大小写和月份：例如 `DCE:m2701`、`CZCE:SR701`、
`CFFEX:IF2612`、`GFEX:lc2701`，不把郑商所三位月份扩展为四位或改为主连。
策略目录继续接受已有 SHFE / INE 期权格式；目录保存不表示柜台支持执行该合约。

```bash
cargo build --release --bin cn_futures_web
cargo run --bin cn_futures_web -- --config /absolute/cn-manager.toml --check-config

# 只读执行库；不需要 PostgreSQL 或交易凭据。
cargo run --bin cn_futures_web -- --config /absolute/cn-manager.toml --snapshot

# 仅对独立、新建的空 Manager 数据库显式初始化。
# CN_FUTURES_CTA_DATABASE_URL 从仓库外的主机凭据环境读取。
cargo run --release --bin cn_futures_web -- --config /absolute/cn-manager.toml --init-db
cargo run --release --bin cn_futures_web -- --config /absolute/cn-manager.toml --bind 127.0.0.1:18211

cd frontend
npm ci
npm run build
```

HTTP API 只监听 loopback，前端保留 `/manager/` 和同源 `/manager/api/` 网关约定。本机 PM2 运行 Manager 和接收器，经 SSH 反向转发及腾讯云既有 Nginx 提供网页；云端单独运行只读导出器。具体路径与启停方式见[部署说明](cn_futures_pm2.md)，不要使用复制来的 Crypto 生产部署脚本操作国内环境。

## 读取与事实边界

执行库（Exec 的 `persist_manager` RocksDB）只有两个追加型列族，key 为 20 位零填充 UTC 微秒时间戳，值为 crypto 的二进制存储格式加国内扩展块，详见[数据契约](native_netting.md#数据契约exec-执行库)：

- `cn_uniform_orders`：订单生命周期（`amount_update = 0`，按 `client_order_id` 取最新一条）、策略成交份额与内部对冲腿；
- `cn_trade_updates`：每笔交易所成交一条，带交易日、成交编号、手数、CNY 手续费、开平和原生订单身份 `session/order_ref`。

读取端要求恰好这两个列族，否则拒绝（旧的 `cn_trading_events` / `cn_trade_facts` / `cn_strategy_ledger` 库不再可读）。Manager 不从执行库读取 Exec 运行态、柜台持仓、通道状态或费率审计，也不按原始事件区分实时与恢复成交。柜台资金另经下文「柜台资金与每日权益」的只读转发取得。

每个 source 复用独立的 Manager secondary，将 catch-up 和跨列族读取串行化；执行 primary 可以一直保持打开和写入。观察缓存按两个列族各自的最后 key 增量解码新记录，统计订单事件数、成交数及每个 `client_order_id` 的事实成交手数；数据库身份或列族替换使缓存失效。近期订单从订单列族尾部倒序取每个订单最新的生命周期记录，近期成交取成交列族尾部。副本的 apply 锁只在单次查询内持有。

`/snapshot` 返回 `last_record_us`（最新记录 key 时间）、`through_us`（记录完整覆盖到的时刻：本机库为读取时刻，副本为已提交的导出切点）、近期订单与成交、订单事件总数与成交总数及副本状态。账户卡持仓由最近一次账户净值快照加其后的事实成交回放得到，不显示 SDK 估值或运行状态；权益来自柜台资金转发，未接入时显示为不可用。合约乘数不在记录中，成本复盘取维护的参考合约或 market-replay 的合约乘数，二者都缺失时乘数留空。

## 柜台资金与每日权益

2026-11-23 按操作员要求恢复总览的权益类指标，并按原生交易日（夜盘归下一交易日）切日，
不使用上海时间零点。数据链路全程只读：

1. Exec 的持仓 IPC `AccountFunds` 携带资金行交易日、上日结算权益 `pre_balance`、
   当日入金／出金及余额、保证金等柜台原值（`mkt_signal` 同步替换 Exec 与 Viz）。
2. 同主机原生 Viz 的 `/account-funds/<manager_source_id>` 按 counter 显式绑定的
   Manager 身份返回实时帧资金，不返回冷启动检查点。
3. 云端导出器配置 `[sync_export].account_funds_url`（仅回环 HTTP 基址）后提供
   `GET /api/native/sync/{source_id}/account-funds`，与增量同步共用 Bearer 令牌和
   导出名单，校验来源与柜台类型后转发；未配置或 Viz 未绑定该来源（如易达）时返回 404，视为没有资金而非故障。
4. Manager 观察循环每 15 秒经同一隧道读取，校验身份与数值后保存每个交易日最后一次
   观察到 `cn_futures_account_equity_daily`（显式执行
   `scripts/apply_native_equity_schema.sql`，启动不运行 DDL；表缺失时仅记日志，
   总览仍按不可用显示）。实时期间至多每分钟写一次，柜台停止应答时立即保存最后一次。

总览口径：权益 = 柜台 `balance`；昨日权益 = 柜台 `pre_balance`（上一交易日结算权益）；
今日盈利 = 权益 − 昨日权益 − 入金 + 出金；盈亏比例以昨日权益为分母；保证金 =
柜台 `curr_margin`，占比以权益为分母；多／空杠杆 = 回放持仓名义金额 ÷ 权益；日期为
柜台资金查询时间。资金在柜台断线、进程未运行或查询超过 60 秒时标记为历史，不计入
合计。Manager 重启或柜台不可达时显示最近一次保存的交易日观察并标记历史。15 日最大
回撤取最近 15 个交易日，逐日收益 = 当日盈利 ÷ 昨日权益，复利成指数后取峰谷跌幅；
本机没有交易日历，相邻记录之间缺少任一工作日（含交易所节假日）时回撤显示未知，
不跨越未观察到的交易日拼接。日期选择列出已保存的交易日和净值快照交易日；所选交易日
使用当日最后一次资金观察及同时保存的持仓名义金额，没有保存持仓时回退到该交易日的
净值快照。易达 Exec 尚未发布账户资金，其权益类指标保持不可用。Manager 不估算权益或
保证金，也不写执行库或柜台。

2026-11-23 部署：本机库显式执行 `apply_native_equity_schema.sql`；本机 Manager 发布
`20261123T071517Z-c6cf0e2-broker-equity`，只重启 `cn-futures-manager`。sh_dev 收盘后同时换装
ctp-trader 与 Viz（`mkt_signal` `86b21d5b`），导出器 `9a826b4` 配置 `account_funds_url`。
创元当日总览：昨日权益 4,993,030.29、权益 4,992,249.09、保证金 78,125.30，状态 live，
交易日 `20261123` 已写入每日表；易达保持不可用；15 日回撤待积累 15 个连续交易日。

国内页面按合约、开仓、平仓、平今、平昨规则计算理论手续费。实际手续费来自记录扩展块（CNY），未报告时留空。

## 状态机对齐

执行端是订单／仓位状态的唯一维护者。Manager 的 `phase` 是读取投影：订单最新生命周期记录的状态（New / PartiallyFilled / Filled / Canceled / Expired）与同一 `client_order_id` 的事实成交手数合并，不触发下单、重试或账本校正。

| 观察状态 | 事实依据 | 不能据此推断 |
| --- | --- | --- |
| `working` | 最新记录为 New，尚无事实成交 | 已挂入交易所或剩余数量可撤 |
| `partially_filled` | 已有事实成交，订单未终结 | 剩余数量一定可撤 |
| `settling` | 已见 Filled，但事实成交手数不足 | 已收齐成交事实 |
| `filled` / `cancelled` / `rejected` | 相应终态（拒单记为 Expired）且成交事实一致 | 撤单数量（记录不含撤单手数） |

完整身份为 `(source_id, trading_day, venue, session, order_ref)`，记录中的 `client_order_id` 是其稳定哈希，原生身份在扩展块中原样保留。成交保留 `(venue, symbol, trading_day, trade_id, side)`；交易所系统号按整数记录，无法解析时为 0。

## API 与验证

复用 `/api/auth/status|register|login|logout|users`。账户授权独立于策略授权：策略配置权不授予账户数据访问，撤销账户授权在下一次请求生效。

| 路径 | 操作 |
| --- | --- |
| `/api/native/sources` | GET 已授权账户，无存储路径 |
| `/api/native/sources/{id}/snapshot` | GET 独立账户观察结果 |
| `/api/native/sources/{id}/cost?trading_day=YYYYMMDD` | GET 完整交易日的真实成交成本对照 |
| `/api/native/sources/{id}/grants` | GET / PUT 账户查看／配置授权 |
| `/api/native/strategies` | GET 可见策略，POST 创建／保存管理配置 |
| `/api/native/strategies/{name}/grants` | GET / PUT 私有策略授权 |

自动化以 `native_futures::records::fixture` 按 Exec 写入格式生成二进制记录，覆盖扩展块编解码、订单折叠与成交关联、终态先于成交、跨市场／报单组身份、活动 primary 持续写入、增量缓存、损坏记录与非两列族执行库拒绝、只追加同步冲突及 secondary 路径隔离。集成测试只接受独立的 `/tmp/cta-schema-test-*` Unix socket 的 PostgreSQL `15433`；验证匿名拒绝、管理员也受可见性边界限制、源授权立即撤销、私有策略和配置授权、整数手数及执行库记录不变。它不加载任何 CTP／易达 SDK。

```bash
cargo fmt --check
cargo test --locked
# 只在自行建立的隔离测试集群中执行：
CTA_SCHEMA_TEST_SOCKET=/tmp/cta-schema-test-EXAMPLE/socket \
  cargo test --locked --test native_manager -- --ignored
```

后续合并建议先向上游提交共用层的小改动（共享请求函数、按市场验证后的策略存储），再合入独立原生模块与构建入口。不要批量重命名 Crypto 类型或复制一套版本化内部 API。保留源库的历史和审计文档，国内运行配置与生产地址不从 Crypto 环境继承。

2026-11-13 本地验证完成：550 项常规 Rust 测试通过，另在独立 PostgreSQL 集群完成 1 项完整 HTTP／权限联测；原有 32 项需特定环境或显式基准的测试仍保持跳过。新增 9 项原生状态／读库回归通过，release 构建、格式检查、Crypto 与国内两种前端构建通过。前端 lint 仅有上游两个既有未使用声明警告。

用 release 程序直接读取同日 SDK `1.502.55.35` 官网公共仿真的真实执行库，得到交易日 `20261112`、17 笔订单、10 笔事实成交，观察状态为 10 笔 `filled`、7 笔 `cancelled`。保留原有 30 条非零持仓，`cu2612` 测试仓归零，快照属于最新进程且已追平，无成交滞后或投影错误；进程状态为 `stopped`。读取前后执行库全部 14 个文件 SHA-256 不变。此样本的交易为 SHFE `cu2612`，同时包含 SHFE / INE 的既有持仓，不构成新版 INE 报单或期权执行验证。本地观察回执在 Git 忽略的 `artifacts/native-verify/receipt.json`，没有部署或启动常驻 Web 服务。（该记录针对 2026-11-20 之前的 JSON 执行库格式。）
