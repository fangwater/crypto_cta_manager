# 国内期货多策略批量目标与归因

最后更新：2026-11-25 UTC。

与 crypto 对齐：**Exec 持有策略账本**，Manager 只登记名单与合约、接收整批策略目标、把完整策略目标交付 Exec，并只读取 Exec 的 `cn_uniform_orders` 记录计算策略 NAV。Manager 不读取 Exec 运行态、柜台持仓或资金快照。Manager 不做内部对冲、不冻结外部需求、不导入成交、不以归因状态挡住下一批目标或执行计划。

实例保留 `01 / 02` 分组，每个因子或品种聚合是独立子策略，同一决策时点的组内全部策略通过一个请求提交。组合、成员、批次收据和目录目标在同一 PostgreSQL 事务内提交；不同进程通过账户行锁串行化。现有 Crypto 的发布、异步归档和 Redis 通道保持其原有契约。

## 数据契约（Exec 执行库）

Exec 执行库只有两个追加型列族，key 为 20 位零填充的 UTC 微秒时间戳，每列族严格递增、不覆盖不删除，格式与 crypto `uniform_orders` / `trade_updates` 相同，值为二进制，库内没有 JSON：

- `cn_uniform_orders`：crypto 存储格式的统一订单，尾部为国内扩展块（`u32 0xFFFF_FFFE` + 标志位：开平、交易日、成交编号、本次成交手数、CNY 手续费、原生订单身份 `session/order_ref`）。`amount_update = 0` 是订单生命周期记录，按 `client_order_id` 取最新一条为当前状态；`amount_update > 0` 是某策略分到的成交份额（`from_key` 为策略名，账户剩余为 `__unallocated__`，手续费按份额分摊）；`ttype = 2` 是 mid 价内部对冲的一条腿（买方 side 1、卖方 side 2，无手续费）。
- `cn_trade_updates`：每笔交易所成交恰好一条，扩展块带成交编号、交易日、手数、手续费、开平和原生订单身份，按 `client_order_id` 与订单关联。

交易所代码为 CFFEX 64、SHFE 65、INE 66、DCE 67、CZCE 68、GFEX 69；合约键为 `交易所:原生合约`。

## Exec 账本与 Manager 锚点

- Exec（ctp-trader / 易达）对每个原生合约维护 `策略 → 手数`，按 mid 内部对冲相反缺口、只按净额下单，并把成交份额和对冲腿写入 `cn_uniform_orders`。某策略在某合约的有符号手数 = 该策略 `amount_update > 0` 记录的买入减卖出之和。
- 锚点是一个 `cn_uniform_orders` 切点：`{source_id, snapshot_us, orders_key, store_id, note}`，`orders_key` 为请求时该列族的最后 key。Manager 的 `Portfolio.positions / account` 是切点时的分配（含 `__unallocated__`），之后不再由 Manager 改写。初始化、rebase 时具名策略手数必须等于折叠到切点的 Exec 记录；`__unallocated__` 与参考价由配置者给出（rebase 用切点分钟收盘价）。Manager 不再核对柜台事实持仓或未结委托。
- 策略 NAV 从锚点出发，按 key 顺序回放切点之后的成交份额（价格 × 手数 × 合约乘数，手续费取份额自身的 CNY 手续费，缺失时按账户费率估算）与对冲腿（同价、零费用）。名单外策略、其他执行库世代或切点不存在时拒绝出报告；已删除（`retired`）的策略仍算名单内，其锚点行与历史成交照常回放。
- 切换到新执行库时，Exec 账本为空：期初持仓全部记为 `__unallocated__`，此后 Exec 的内部对冲按 mid 把它转给各策略。

## 接口

接口支持国内 Manager 原有 `cn_cta_session` 登录与授权；机器令牌仅允许下文列出的组级发布操作。需要账户权限和名单内每一个策略的对应权限。策略先通过 `/api/native/strategies` 创建并授权，加入组合后，该接口拒绝单条目标保存，必须使用整批入口。

| 方法与路径 | 行为 |
|---|---|
| POST `/api/native/sources/{id}/netting` | 显式初始化一次，在当前切点核对具名策略与 Exec 记录；禁止隐式覆盖旧组合 |
| POST `/api/native/sources/{id}/netting/extend-scope` | 注册新组，或向现有组添加策略和零仓合约（Exec 记录中须尚无其归属） |
| POST `/api/native/sources/{id}/netting/rebase` | 管理员以明确的期初持仓开启新的账户和策略净值起点 |
| POST `/api/native/sources/{id}/netting/stop` | 停止并清仓：所列策略目标立即置零并生成新的执行计划 |
| POST `/api/native/sources/{id}/netting/retire` | 删除配置：Exec 账本已清仓的已停止策略移出名单，空组连同发布令牌一并删除 |
| GET `/api/native/sources/{id}/netting` | 组合（锚点分配）、完整净目标、`stopped` / `retired`，以及 `exec_ledger`（折叠全部 Exec 记录得到的当前具名策略手数） |
| POST `/api/native/sources/{id}/target-batches` | 原子替换指定组全部策略的完整目标 |
| GET `/api/native/sources/{id}/netting/events?after=-1&limit=50` | 按 revision 分页读取不可变审计，最多 100 条 |
| GET `/api/native/sources/{id}/netting/execution-plan` | 当前完整策略目标与净目标 |

初始化示例：

```json
{
  "strategy_groups": {"cn-futures-01": ["cn-futures-01-td_pr_010-1h", "cn-futures-01-factor_094-1h"]},
  "contracts": {"SHFE:sn2612": {"multiplier": 1}},
  "positions": {},
  "note": "已核对当前账户空仓与 Exec 记录"
}
```

`positions` 为 `策略 → 合约 → {lots, price}`，可含 `__unallocated__`；具名策略之外的持仓放在这里，合计即锚点时账户净手数（由配置者负责，Manager 不读柜台核对）。

整批目标示例（时间戳须替换为当前决策的 UTC 微秒时间）：

```json
{
  "strategy_group": "cn-futures-01",
  "batch_id": "sn-hour-20261117-2100",
  "expected_revision": 0,
  "decision_us": 1794920400000000,
  "trading_day": "20261117",
  "strategies": {
    "cn-futures-01-td_pr_010-1h": {"SHFE:sn2612": 1},
    "cn-futures-01-factor_094-1h": {"SHFE:sn2612": -1}
  }
}
```

每批指定 `strategy_group`，必须包含该组全部登记策略，每个策略必须包含全部登记合约，包括显式零目标。移除因子或换月时保留旧身份、旧合约及必要的零目标。批次不再携带 BBO，内部对冲由 Exec 用实时行情定价。

`batch_id` 在账户内幂等：完全相同的请求重试返回原收据，即使后续批次已经提交。相同编号内容不同、revision 不一致或决策时间回退返回 409。新批次决策不得早于锚点，必须在接收时最近 120 秒内。接收批次不读取 Exec 副本，也不等待上一批成交或归因；交易日由 Exec 接纳目标时核对。

扩充范围使用会话授权的 `extend-scope`，请求包含 `event_id`、`expected_revision`、`strategy_group`、新增 `members`、新增 `contracts` 和 `note`。服务端核对旧锚点仍属同一执行库，在当前切点确认 Exec 记录中新合约和新成员均无具名归属，再原子写入扩充事件；新合约从该切点起计入策略 NAV。

重设净值起点显式提供 `event_id`、`expected_revision`、各组成员 `strategy_groups`、按策略（可含 `__unallocated__`）和合约分配的有符号期初手数 `positions`、期初原生交易日 `trading_day`、账户期初今昨／多空持仓 `inventory`（`[{venue,symbol,side,today,yesterday}]`，不支持同合约双向）以及 `note`。各合约策略手数之和须等于 `inventory` 净手数，具名策略手数须等于切点的 Exec 记录。按切点的已完成分钟收盘价计价，原子保存账户净值快照和新的归因起点。原始执行事实和旧审计记录保留，发布令牌和执行 revision 连续。

## 停止并清仓与删除配置

国内组合与 crypto 的「停止并清仓 → 删除配置」两步对齐。两个接口都使用会话授权，需要账户配置权限和所列每个策略的配置权限；请求格式相同，`event_id` 在账户内幂等（相同内容重试返回原收据，内容不同返回 409），`expected_revision` 不一致返回 409：

```json
{
  "event_id": "halt-td_pr_010-20261125",
  "expected_revision": 41,
  "strategy_group": "cn-futures-01",
  "members": ["cn-futures-01-td_pr_010-1h"],
  "note": "因子下线，停止并清仓"
}
```

`event_id` 只含字母、数字和 `_-.:`，加上 `stop:` / `retire:` 前缀后不超过 128 字节；`note` 非空且不超过 4096 字节；`members` 非空，且都属于 `strategy_group`。

**停止**（`/netting/stop`）：所列策略记入组合的 `stopped`，当前目标全部置零，revision 与 `target_revision` 同时加一，`batch_id` 为 `stop:<event_id>`，计划决策时间为 Manager 接收时间，交易日沿用最近一次实盘批次的 `trading_day`，因此不等发布端下一批即交付 Exec。已停止的策略仍在名单内：之后该组的 `target-batches` 仍须包含它们（现有发布端无需改动），但 Manager 无论提交值如何都把它们的目标强制为零，收据与执行计划中可见。Exec 继续持有其手数并以零目标平仓，平仓成交仍归属原策略。重复停止已停止的策略被拒绝；不支持撤销停止。收据：

```json
{
  "kind": "strategy_stop",
  "source_id": "cn_ctp_01",
  "event_id": "halt-td_pr_010-20261125",
  "revision": 42,
  "batch_id": "stop:halt-td_pr_010-20261125",
  "strategy_group": "cn-futures-01",
  "members": ["cn-futures-01-td_pr_010-1h"],
  "note": "因子下线，停止并清仓",
  "decision_us": 1795593600000000,
  "trading_day": "20261125",
  "stopped": ["cn-futures-01-td_pr_010-1h"],
  "retired": [],
  "net_targets": {"SHFE:sn2612": -1},
  "execution_plan": {
    "source_id": "cn_ctp_01",
    "batch_id": "stop:halt-td_pr_010-20261125",
    "target_revision": 42,
    "account_revision": 42,
    "decision_us": 1795593600000000,
    "trading_day": "20261125",
    "strategies": {
      "cn-futures-01-td_pr_010-1h": {"SHFE:sn2612": 0},
      "cn-futures-01-factor_094-1h": {"SHFE:sn2612": -1}
    },
    "stopped": ["cn-futures-01-td_pr_010-1h"],
    "net_targets": {"SHFE:sn2612": -1}
  }
}
```

**删除**（`/netting/retire`）：只接受已停止的策略。服务端核对旧锚点仍属同一执行库，在新的 `cn_uniform_orders` 切点折叠 Exec 账本（与 extend-scope / rebase 相同），所列策略在任一合约仍有手数时返回 409。通过后在同一事务内：把它们移出 `targets`、组成员和 `stopped`，记入 `retired`；清空其目录目标；组变空时删除该组并撤销该组发布令牌（收据带 `removed_group`）；生成 `batch_id` 为 `retire:<event_id>` 的新执行计划。策略名仍归属本账户（成员归属表不删除），不可加入其他账户；可以用 extend-scope 重新加入本账户的任意组，只要 Exec 账本显示它已平仓：它移出 `retired`、目标从 0 开始，锚点行沿用，NAV 接着回放它的成交。其锚点 `positions` 行保留，账户核对不变。收据与停止相同，`kind` 为 `strategy_retire`，另含 `cut`（折叠所用切点）：

```json
{
  "kind": "strategy_retire",
  "source_id": "cn_ctp_01",
  "event_id": "drop-td_pr_010-20261126",
  "revision": 57,
  "batch_id": "retire:drop-td_pr_010-20261126",
  "strategy_group": "cn-futures-01",
  "members": ["cn-futures-01-td_pr_010-1h"],
  "note": "Exec 账本已清仓，删除配置",
  "decision_us": 1795680000000000,
  "trading_day": "20261126",
  "stopped": [],
  "retired": ["cn-futures-01-td_pr_010-1h"],
  "cut": {"source_id": "cn_ctp_01", "snapshot_us": 1795680000000000, "orders_key": 1795679990000000, "store_id": "sync:…", "note": "Exec 账本已清仓，删除配置"},
  "net_targets": {"SHFE:sn2612": -1},
  "execution_plan": {
    "source_id": "cn_ctp_01",
    "batch_id": "retire:drop-td_pr_010-20261126",
    "target_revision": 57,
    "account_revision": 57,
    "decision_us": 1795680000000000,
    "trading_day": "20261126",
    "strategies": {"cn-futures-01-factor_094-1h": {"SHFE:sn2612": -1}},
    "net_targets": {"SHFE:sn2612": -1}
  }
}
```

全部策略删除后，组、名单可以为空，合约仍保留登记；`net_targets` 各合约为 0，执行计划仍为 `ready`，`strategies` 为空、gRPC `strategy_targets` 为零条，Exec 据此清空账户。之后可用 extend-scope 注册新组；rebase 的 `strategy_groups` 须与当前（可能已减少或为空）的组集合一致，`stopped` 跨 rebase 保留并保持零目标。策略 NAV 回放把 `strategy_stop` / `strategy_retire` 事件视为不移动持仓；参考净值把它们的 `net_targets` 视同一批目标。前端「策略启用」页对国内账户逐策略提供「停止并清仓」，对 Exec 账本已清仓的已停止策略提供「删除配置」，其余编辑保持禁用。

## 组级发布与执行计划

`Initialize.strategy_groups` 把名单按实例划分，例如 `cn-futures-01` 和 `cn-futures-02`。每个子策略只属于一个组；某组提交只替换该组完整目标，其他组保持原值；决策时钟按组校验。

`signal_times` 是可选的逐策略真实决策时间表；提供时必须完整对应本批策略，数值不晚于外层发布时间。

| 方法与路径 | 行为 |
|---|---|
| POST `/api/native/sources/{id}/netting/publish-token` | 会话授权，为 `strategy_group` 生成/轮换一次性展示的机器发布令牌 |
| DELETE 同一路径 | 会话授权，撤销该组发布令牌 |
| GET `/api/native/sources/{id}/netting/publication?strategy_group=cn-futures-01` | 组名单、本组 `stopped`、完整合约范围、组时钟和当前账户 revision |

令牌接口请求为 `{"strategy_group":"cn-futures-01"}`。机器客户端使用 `X-CTA-Publish-Token`，令牌绑定源账户、策略组和发放用户；机器令牌只能读取该组发布元数据和整批 POST。库内只保存哈希。

整批 POST 的收据包含 `net_targets`（每个原生合约的带符号净手数）和 `execution_plan`：`source_id / batch_id / target_revision / account_revision / decision_us / trading_day / strategies / stopped / net_targets`。`strategies` 是账户全部登记策略的完整目标；Exec 以 `source_id + batch_id + target_revision` 去重并整份接纳。

## 执行计划交付

配置 `exec_plan_readers` 时，启用的 CTP 和易达来源都可使用各自的计划读取令牌。gRPC `ManagerPlans.Watch` 交付 `PositionTarget.strategy_targets`（完整策略名单，每个策略相同的合约向量）；`GET /api/native/sources/{id}/netting/exec-plan` 提供同一目标 JSON 及 `X-CTA-Plan-Status`、`X-CTA-Plan-Batch-Id`、`X-CTA-Plan-Target-Revision`，`204` 的响应体为空。

| 状态 | 含义 |
|---|---|
| `ready` | 当前计划带原生交易日且决策时间不在未来，交给 Exec 准入；名单为空时同样就绪 |
| `no_batch` | 尚无已提交批次 |
| `observe_only` | 只读订阅收到的当前目标观测，执行有效期为零 |
| `missing_trading_day` / `decision_in_future` | 计划缺少原生交易日，或决策时间在未来 |

计划自带决策时间与交易日：整批取该批 `decision_us` 与 `trading_day`；停止、删除取事件接收时间，并沿用最近一次实盘批次的交易日（从未有过实盘批次时为 `missing_trading_day`，不会把分析账户变成实盘清仓）。旧状态没有计划时间时按最后提交组的组时钟推导。计划没有租约，直到下一批或下一次停止／删除替换。计划交付不读取 Exec 副本、账户快照或归因状态，因此副本滞后（`replication_stale`）不会阻塞执行。账户、交易日、合约规则和风控由 Exec 在接纳目标与下单时校验。

## 数据维护与验证

新库由当前 `migrations/schema.sql` 显式初始化。已有国内 Manager 数据库首次启用净额归因时审核并执行 `scripts/apply_native_netting_schema.sql`；切换到 Exec 账本时审核并执行 `scripts/apply_exec_owned_attribution_schema.sql`，删除不再使用的外部需求预算表。启用停止／删除前审核并执行 `scripts/apply_native_netting_retirement_schema.sql`，只放宽事件种类约束（`strategy_stop` / `strategy_retire`），`stopped` / `retired` 保存在组合 JSON 状态内，旧状态无需改写。旧格式的组合、事件和净值快照引用已退役的执行库与证据序号，无法读取；切换时由运维清空这些行后重新初始化。

数据流：Exec 在业务循环内把证据推给同目录的 `persist_manager`，写本机 `data/persist_manager`；Manager 的同步 export（Exec 主机）以 secondary 读取并分页提供，中心 Manager 的 collector 主动拉取到自有副本。同步只复制 `cn_uniform_orders` 与 `cn_trade_updates`，两者都只追加，覆盖或删除已有 key 即冲突；读取端要求执行库恰好有这两个列族（Manager 副本另有 staging）。export 状态键升为 `meta/head/v3` / `meta/pending/v3`，切换时每个国内 source 须清空旧 export 状态与副本后重新全量拉取，期间只影响 Manager 展示与 NAV，不影响交易。本次代码改动不自动执行线上 DDL 或重启服务。

SDK-free 验证：

```bash
cargo check --locked --lib --bin cn_futures_web
cargo test --locked --lib native_futures
CTA_SCHEMA_TEST_SOCKET=/tmp/cta-schema-test-... cargo test --locked --test native_manager --test native_netting --test native_nav -- --ignored
```

数据库测试只接受独立 `/tmp/cta-schema-test-*` Unix socket、15433 端口，并创建自己的临时数据库；验证幂等、并发 revision、整批回滚、权限与发布令牌，以及由 Exec 二进制记录回放的策略 NAV。测试数据由 `native_futures::records::fixture` 按 Exec 写入格式生成。
