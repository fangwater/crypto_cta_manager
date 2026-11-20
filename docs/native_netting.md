# 国内期货多策略批量目标与内部轧差

最后更新：2026-11-18 UTC。

国内 Manager 提供账户隔离的策略归因账本。实例保留 `01 / 02` 分组，临时归因使用 `cn-futures-01-{factor_name}-1h`；每个因子是独立子策略，同一决策时点的所有策略目标通过一个请求提交。配置目录、当前归因账本、成对内部调拨、冻结的外部需求和批次收据在同一个 PostgreSQL 事务内提交；提交前读者继续看到上一批。不同进程也通过账户行锁串行化。没有逐策略执行通知。

本模块不加载交易 SDK、不修改执行库、不报撤单。读取 API 输出完整的净目标、外部需求开关和 revision，供后续执行端对接；`execution_enabled=false`。现有 Crypto 的发布、异步归档和 Redis 通道保持其原有契约。国内新接口的 200 表示 Manager 归因事务已提交，不表示交易所已成交。

## 账本与匹配

- 每个账户固定登记策略组、各组完整策略名单和原生合约范围，合约键如 `SHFE:sn2612`。所有目标为有符号整数手。不同账户、交易所、原生月份互不轧差。
- 初始化需要当前运行代、完整且无成交滞后的事实持仓快照和明确的人工策略分配。客户端提供 `snapshot_evidence_sequence`，服务端重新读取快照并核对，冻结执行库世代和原生证据 SHA-256；后续批次及成交导入也核验此锚点。初始策略净手数必须等于对应合约的事实账户净手数，不按目标或合约相似度猜测归属。初始参考价和合约乘数由配置者明确指定并审计，是归因起点，不代表历史实际成本或账户结算净值。
- 未确认结算的只读账户可以使用新鲜、连接有效且完整的 `reconciled` 证据初始化归因；交易 `ready=false` 保留。普通未就绪 `state`、已停止、断连或恢复中的进程均不能建立锚点。归因初始化不确认结算、不授予交易权限。
- 对每个策略计算 `目标 - 已归属持仓`。同合约的相反需求按策略名确定性匹配。每笔内部事件同时记录 buyer / seller、手数、同一价格、BBO 来源和观测时间，双方原子入账，手续费为零。价格固定为该决策时点有效 BBO 的 mid；BBO 不得晚于决策，也不得早于决策五秒以上。没有有效报价则整批拒绝。
- 内部调拨只改变策略 FIFO 成本和实现收益，不改变账户事实归因持仓，不生成交易所成交、委托编号或执行库记录。策略可以持有相反虚拟仓位，账户保持净额。
- 未抵消部分保持外部需求。`external_demand_open=true` 期间拒绝下一批目标，避免旧委托尚未结清时再次调拨策略归属。接收目标不会把未成交需求写成持仓。实际成交只能通过源执行库的 `cn_trade_facts` 原始 key 导入，价格、数量、费用、身份和证据序号全部从该账户的事实记录读取。使用共享原生证据模块的有界原始回报校验；仅有恢复查询或无法证实原始实时回报的记录不能当作锚点后的新成交。
- 实际成交归属必须显式指定批次与策略；冻结的外部需求限制可分配方向和数量。既不按时间接近推定策略，也不自动把账户其他成交套到当前因子。成交身份在账户内唯一，跨事件编号重发同一成交也会被拒绝。
- 部分成交只消耗相应需求；晚到成交继续关联原批次，需求结清之前不接收下一批目标。已撤销的未成交部分必须通过独立释放接口，以后续新鲜、完整、无委托且与归因持仓一致的源状态为证据释放；HTTP 超时或仅发出撤单请求不能作为完成依据。实际费用按分配手数比例分摊，最后一项吸收浮点余差；未知或非 CNY 手续费保持未知。账户事实的今昨、多空、开平属性不被虚拟 FIFO 改写。

`Σ策略归属手数 = 账户归因手数` 在每次提交前校验。同一参考价下，各策略 FIFO 损益之和与账户归因损益一致；内部调拨改变各策略的实现/浮动分解，其合计保持一致。归因视图使用显式锚点及已导入事实成交，不冒充完整账户权益，也不包含未导入成交、出入金、结算调整或未知费用。

例如 A 原来持有多头一手，新批次要求 A=0、B=1：Manager 只记 A 虚拟卖出/B 虚拟买入各一手，账户净目标不变，外部需求为零。

## 接口

接口支持国内 Manager 原有 `cn_cta_session` 登录与授权；机器令牌仅允许下文列出的组级发布操作。需要账户权限和名单内每一个策略的对应权限；查看事件或估值同样检查整个名单的可见性。策略先通过 `/api/native/strategies` 创建并授权，加入组合后，该接口拒绝单条目标保存，必须使用整批入口。

| 方法与路径 | 行为 |
|---|---|
| POST `/api/native/sources/{id}/netting` | 显式初始化一次，核对事实快照；禁止隐式覆盖旧账本 |
| POST `/api/native/sources/{id}/netting/extend-scope` | 用新鲜的对账快照注册新组，或向现有组添加策略和零仓合约 |
| POST `/api/native/sources/{id}/netting/rebase` | 管理员以无未结委托的柜台快照开启新的账户和策略净值，并为每个原有组保留一个策略 |
| GET `/api/native/sources/{id}/netting` | 当前策略 FIFO、完整净目标、revision 和已归属手数 |
| POST `/api/native/sources/{id}/target-batches` | 原子更新指定组全部策略的完整目标 |
| GET `/api/native/sources/{id}/netting/events?after=-1&limit=50` | 按 revision 分页读取不可变审计，最多 100 条 |
| POST `/api/native/sources/{id}/netting/fill-allocations` | 引用源库事实记录，将真实成交分配到冻结需求 |
| POST `/api/native/sources/{id}/netting/ctaker-fill-allocations` | 核对 CTP 或易达的原生报单、成交与目标批次后，从冻结需求分配成交 |
| POST `/api/native/sources/{id}/netting/release-demands` | 用新的完整、无委托且持仓一致的事实快照释放已撤销的剩余需求 |
| POST `/api/native/sources/{id}/netting/valuation` | 用完整、有效的报价统一估值，返回策略和账户归因 FIFO 损益 |

初始化示例（空仓也必须由服务端快照证实）：

```json
{
  "strategy_groups": {"cn-futures-01": ["cn-futures-01-td_pr_010-1h", "cn-futures-01-factor_094-1h"]},
  "contracts": {"SHFE:sn2612": {"multiplier": 1}},
  "positions": {},
  "snapshot_evidence_sequence": 123,
  "note": "已核对当前账户空仓；按用户表开始独立因子归因"
}
```

整批目标示例（时间戳须替换为当前决策与实际 BBO 的 UTC 微秒时间）：

```json
{
  "strategy_group": "cn-futures-01",
  "batch_id": "sn-hour-20261117-2100",
  "expected_revision": 0,
  "decision_us": 1794920400000000,
  "strategies": {
    "cn-futures-01-td_pr_010-1h": {"SHFE:sn2612": 1},
    "cn-futures-01-factor_094-1h": {"SHFE:sn2612": -1}
  },
  "quotes": {
    "SHFE:sn2612": {
      "bid": 250000,
      "ask": 250010,
      "observed_us": 1794920400000000,
      "source": "company_zmq"
    }
  }
}
```

每批指定 `strategy_group`，必须包含该组全部登记策略，每个策略必须包含全部登记合约，包括显式零目标。移除因子或换月时保留旧身份、旧合约及必要的零目标。

扩充范围使用会话授权的 `extend-scope`，请求包含 `event_id`、`expected_revision`、
`strategy_group`、新增 `members`、新增 `contracts`、`snapshot_evidence_sequence` 和 `note`。
新组至少登记一个成员。先创建并授权新增目录策略。服务端核对旧归因锚点、新鲜且完整的同源事实快照、新合约无多空持仓、
无未决委托或外部需求，再原子写入扩充事件。现有 FIFO、策略目标、归因锚点和目标批次保留，
新增维度初始化为零，账户 revision 递增。已有数据库先显式应用
`scripts/apply_native_netting_scope_schema.sql`。

`batch_id` 在账户内幂等：完全相同的请求重试返回原收据，即使后续批次已经提交。相同编号内容不同、revision 不一致或决策时间回退返回 409。新批次决策不得早于锚点，必须在接收时最近 120 秒内；幂等重试先查已提交事件，不受后续时间推移影响。任何校验或存储失败都回滚整批。

重设净值起点显式提供 `event_id`、`expected_revision`、`snapshot_evidence_sequence`、各组单成员
`strategy_groups`、按策略和合约分配的有符号期初手数 `positions` 以及 `note`。按快照时点的
已完成合约分钟收盘价给现有柜台仓位计价，原子保存账户快照和新的归因起点。新净目标从
事实期初仓位起步。原始执行事实和旧审计记录保留，正常查询从新起点开始，发布令牌和执行
revision 连续。旧目标租约结束且没有未结执行需求后才接受重设。

真实成交导入示例：

```json
{
  "event_id": "fill-allocation-001",
  "batch_id": "sn-hour-20261117-2100",
  "expected_revision": 1,
  "evidence_key": "303031",
  "allocations": {"cn-futures-01-td_pr_010-1h": 1}
}
```

`evidence_key` 是该 source 的 `cn_trade_facts` 二进制 key 的十六进制，不是交易所订单号。示例批次双方全部内部抵消，没有可分配的外部需求，因此此成交示例须用于确实存在买入剩余需求的另一批次。真实执行对接时必须从报单前的批次身份一路保留到事实成交，不能用“同合约且时间接近”建立自动关联。执行端应整体消费并校验 revision，以真实账户持仓、已有委托及撤单未决量计算委托；当前 API 不替执行端管理报撤单状态。

释放请求为 `{"event_id":"cancel-confirmed-001","expected_revision":2,"note":"撤单确认后，用新持仓与无委托状态核验剩余需求"}`。释放只关闭外部需求，不清零策略目标、不改持仓、不发撤单；下一批明确目标才可开启新需求。执行端只能消费 `external_demand_open=true` 的当前完整计划，不能因为释放后的残余目标非零就自动恢复已撤销批次。

估值请求为 `{"at_us": <UTC微秒>, "quotes": {<原生合约>: <BBO对象>}}`，明确标识为虚拟策略归因。原账户 NAV 和成本复盘接口继续读取原始事实，不被内部成交污染。

## 组级发布与整批净目标

`Initialize.strategy_groups` 把名单按实例划分，例如 `cn-futures-01` 和 `cn-futures-02`。每个子策略只属于一个组；同一账户的批次事务仍串行化。某组提交只替换该组完整目标，其他组保持原值；决策时钟按组校验，两个组可以使用同一个 bar 时间。未结清的账户外部需求仍阻止下一批，防止未知旧委托结果与新调拨交叉。

`signal_times` 是可选的逐策略真实决策时间表；提供时必须完整对应本批策略，数值不晚于外层发布时间。内部调拨定价继续使用外层 `decision_us` 的有效 BBO，不把旧 bar 的参考价当作当前调拨价。

| 方法与路径 | 行为 |
|---|---|
| POST `/api/native/sources/{id}/netting/publish-token` | 会话授权，为 `strategy_group` 生成/轮换一次性展示的机器发布令牌 |
| DELETE 同一路径 | 会话授权，撤销该组发布令牌 |
| GET `/api/native/sources/{id}/netting/publication?strategy_group=cn-futures-01` | 组名单、完整合约范围、组时钟和当前账户 revision |
| GET `/api/native/sources/{id}/netting/execution-plan` | 一份完整账户净目标及其子策略归因分配 |

令牌接口请求为 `{"strategy_group":"cn-futures-01"}`。机器客户端使用 `X-CTA-Publish-Token`，令牌绑定源账户、策略组和发放用户；每次检查当前用户状态、账户边界及组内全部策略配置权。机器令牌只能读取该组发布元数据和整批 POST，不能读其他组、整个归因账本、生成令牌或导入成交。权限撤回、用户禁用、令牌轮换/撤销在下一请求生效。库内只保存哈希。

整批 POST 的收据新增 `execution_plan`，与目标和内部调拨在同一事务内冻结；查询接口提供同一账户当前完整计划。字段包括 `source_id / batch_id / target_revision / account_revision / publishable / net_targets / attribution`。`net_targets` 只含每个原生合约的一份带符号净手数，不按因子发多份执行命令。`attribution` 保存各策略剩余需求；成交仅更新账户 revision，保留该批 target revision。后续执行端应以 `source_id + batch_id + target_revision` 去重，并整份接纳目标。

当剩余外部需求为零时，计划不可对外执行，内部仍可形成双方调拨记录；净目标为零但当前账户尚有仓位时，平仓需求仍可执行。释放已撤销需求会将对应组置为持仓保持状态；别的组后续提交不会自动恢复该组旧的未成交意图，只有该组新的明确批次可以恢复。

本轮已完成信号整批推送到 Manager、组隔离、净目标输出和可拆分归因。按用户确认，不新增原生自动报撤单适配器，不把计划接纳当作实际成交；`execution_enabled=false` 仍然准确。

## 执行计划状态

配置 `ctaker_plan_readers` 时，启用的 CTP 和易达来源都可使用各自的计划读取令牌。

`GET /api/native/sources/{id}/netting/ctaker-plan` 在已鉴权的计划响应中提供 `X-CTA-Plan-Status`。已知批次还带有 `X-CTA-Plan-Batch-Id` 和 `X-CTA-Plan-Target-Revision`。`200` 保留原有完整目标 JSON，`204` 的响应体为空。gRPC `ManagerPlans.Watch` 在 `PlanUpdate.status`、`batch_id` 和 `target_revision` 中提供相同信息。

| 状态 | 含义 |
|---|---|
| `ready` | 当前批次通过账户和有效期校验，可以交给 Exec 准入 |
| `no_batch` / `no_external_demand` | 尚无已提交批次，或当前批次没有外部需求 |
| `observe_only` | 只读订阅收到有外部需求的目标观测，执行有效期为零 |
| `missing_trading_day` / `decision_in_future` / `lease_expired` | 批次缺少原生交易日、决策时间在未来，或尚有需求但执行有效期已结束 |
| `replica_not_ready` / `account_not_ready` / `account_snapshot_stale` | 副本不可用或滞后、账户尚未就绪，或账户快照超出有效期 |
| `trading_day_mismatch` / `account_positions_unconfirmed` / `evidence_error` | 交易日不匹配、事实仓位未与账本确认，或证据校验失败 |
| `refreshing` | 读取期间账户 revision 变化，Manager 正在重查 |

执行订阅只交付 `ready` 的完整目标。有效期结束或账户快照过期时，Manager 重新检查并在状态变化时通知订阅方。Exec 按批次和状态变化记录 `manager_plan_status`，正常空闲记为 `info`，阻塞状态记为 `warn`。只读订阅仍可收到无外部需求的零有效期目标。已接纳的目标继续受原有有效期和 Exec 风控约束。

## 数据维护与验证

新库由当前 `migrations/schema.sql` 显式初始化；已有国内 Manager 数据库首次启用净额归因时须审核并执行 `scripts/apply_native_netting_schema.sql`，不在正常启动时执行 DDL。该脚本只创建 Manager 自有组合、成员、事件、外部需求预算和组级发布令牌表，执行库不变。已安装旧净额表或存在归因账本的数据库需要单独审核迁移，不能重跑建表脚本或覆盖账本。本次代码改动不自动执行线上 DDL 或重启服务。

SDK-free 验证：

```bash
cargo check --locked --lib --bin cn_futures_web
cargo test --locked --lib native_futures::netting::tests
CTA_SCHEMA_TEST_SOCKET=/tmp/cta-schema-test-... cargo test --locked --test native_netting -- --ignored
```

数据库测试只接受独立 `/tmp/cta-schema-test-*` Unix socket、15433 端口，并创建自己的临时数据库；验证幂等、并发 revision、整批回滚、跨编号成交去重、需求预算和连接重建后恢复。
