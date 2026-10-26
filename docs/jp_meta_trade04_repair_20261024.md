# jp-meta trade04 漏成交与未分配持仓修复（2026-10-24）

后续进展：本报告之后已完成 prc 历史归属重建，见
[历史重建与正式页面核验](jp_meta_prc_historical_ownership_20261024.md)。
本报告中的时间窗口和发布结论保留为当时的审计事实。

目标为 `binance_exec_trade04`，主机 `jp-meta-elvpn`，Exec RocksDB
`/home/ubuntu/binance_exec_trade04/data/persist_manager`，IDENTITY
`9eaba644-adce-4e1f-8b57-926f104e3035`。下列时间均为上海时间。
操作者明确允许维护后，已完成两批原生离线补录及一条不可变归属校准。
没有修改 9/23 的账户/策略起仓、策略参数、交易 IP 或原始成交归属。

## 查实与处理

发现两个不同问题，不能把它们都归因于同一个重启时间：

1. 10/18 04:25:03.344–04:26:22.517 有 25 笔普通 Maker 成交未保存，
   涉及 22 个订单、17 个合约，成交名义 1690.65528660 USDT。
   根据同 source、symbol、client ID 高位的唯一历史归属，18 笔恢复为
   `chase_exec:rbf_small`，7 笔恢复为 `chase_exec:rbf_big`。
   保留原正数 client ID、交易所订单/成交身份、时间、数量、价格与 Maker 事实。
   归属是有历史证据的推断，不声称找回不存在的原始 WS 字节。
2. 10/18 16:45:17 的启动日志显示恢复了 10/13 的 Batch/Chase 策略账，
   此后产生真实 `SYSTEM_POSITION_CLOSE` 成交，导致 Manager 的策略持仓与
   当前运行账不一致。保留这些真实历史成交与历史盈亏，通过有观察依据的
   新时点归属转移修复当前持仓，不能倒改原始起仓或抹去历史亏损。

扩展到 9/23 起仓后的完整同源历史后，还发现 68 个 `ios_` 手机端减仓订单、
71 笔 MARKET/Taker 成交，名义 70.82236330 USDT。它们没有 CTA 身份，
单独以 `external_account_fill:ios` 补录；审计保留原始字符串 client ID，
原生格式按已审计契约使用负交易所订单 ID 占位，不伪造正数 Exec ID 或 CTA 标签。
原币佣金独立保留，Manager NAV 仍使用已有 Maker/Taker 估计费率。

第一轮全历史交易所扫描完成 234 个合约 × 6 个窗口，共 1404 个分页范围，
348605 笔成交、291625 个订单。扣除已恢复的 25 笔后，剩余缺口正是上述
71 笔外部账户成交；本地已保存原始回报与 uniform 正数量之间未发现另一个缺口。
后续 235 个 source 历史/当前合约的两个连续范围，又逐笔核对 210 + 141 笔成交，
覆盖至归属观察时点 17:20:00.899，全部匹配，没有维护窗口或校准前新增缺口。
核对以交易所 order ID → 原 client ID → 原始 trade 时间/事件时间 → uniform
为链路，逐笔一次性匹配 side、quantity、price；不把交易时间与事件时间混为一谈。
不声称查询过该账户从未出现在 source 历史/当前合约集合中的全部零仓市场。

## 两次补录

Manager 没有写入 Exec RocksDB。使用在 `mkt_signal` 当前 `arbmm` 分支本地构建、
校验 SHA 后上传的独立 `native_fill_import`。在线 Exec 二进制没有更换。
依该部署的官方 wrappers 停止生产者，保留 PM 排空，确认稳定尾部、持久化和独占锁，
再停 PM、生成库外完整 checkpoint，使用同步原子批次写入并恢复原服务。
没有删除 LOCK、开启第二个 live primary、发送修复 IPC 或撤单。

| 批次 | 服务维护区间 | 新增 uniform / trade / order | 重复 dry-run |
|---|---|---:|---:|
| 策略 25 笔 | 16:28:53.329848–16:31:59.796566 | 25 / 25 / 0 | 0 新增，25 已存在 |
| 手机 71 笔 | 16:57:30.818113–17:01:27.260394 | 71 / 71 / 0 | 0 新增，71 已存在 |

第二次交易生产者在 17:01:13.172690 已恢复。两次启动监视均未出现异常集中系统平仓。
另一次准备检查遇到活动订单而退出，没有停止服务；保留自然空闲等待记录。

批次 ID：

- 25 笔：`212cebed62d773680a688c76507b9f129d0aad6bb989bf7238a51080bc8af12a`
- 71 笔：`5d31c69a00d1b0099dae7e4fb6bbee1a60a9c0194c0db2d5773708c7817abbd2`

完整 checkpoint 保留在远端 Manager 备份目录：

- `backups/trade04_native_import_ckpt_20261024T162853+0800`
- `backups/trade04_external_fills_ckpt_20261024T165730+0800`

## 当前归属校准

校准时点为 **17:20:00.899**，使用 17:19 完成的一分钟 K 线收盘价。
前后两次明确绑定当前交易 IP 的签名账户查询保留原始十进制数量，
931 个观察均相同，且包围无活动订单的 Viz 时间点。完整 RocksDB + 原始起仓回放
与全部账户观察严格匹配，门槛仍为 `<1e-8`。

同时读取本 source 命名空间的完整 Batch/Chase 策略账；前后 positions 相同，
每条 Viz 行均在对应账中核对。Viz 的 `chase_exec` 对应持久账的 `chase` family。
保留真实相抵策略仓位。对两本账都未覆盖的市场保留历史所有者，
因此 ZIL、BASED、ORDER 的原归属没有被误清成未分配；明确保留的零条目可以证明
已退役目标归零。仅依 Viz 缺行生成的早期候选已拒绝，从未写入。

最终候选共 424 行、139 个合约/venue，逐市场转移零和，显式审核 residual 腿，
但残差始终由账户减命名策略计算。校准涉及的未分配名义敞口在该分钟价下从
41638.51521118588 降到 8.093688137968856 USDT，主要保留 FOLKS 的真实小残余。
这两个数是持仓名义金额，不是亏损。

候选 SHA256：`6493fc4482198b1ac939341c1eaafe0121849b9dd376be2c6b949f53647077dd`。
正式工具再次回放前值、账户 FIFO、全历史/7D/1D/after 窗口及策略相加后，
插入 `cta_strategy_allocation_snapshots` 一条记录；没有 DDL。
落库 JSON 数字经过原生 f64 解析/序列化，64 个字段有不超过 2 ULP 的差异，
最大绝对差 7.28e-12；身份、时点、条目一致。这不是放宽账户匹配或校准前值门槛。

校准从此时点影响后续策略归属和资金费估计，不能描述为已重写过去一周的归属。
重启后真实系统平仓及没有身份的手机成交的历史盈亏仍保留。

## 发布和线上验收

Manager `cta_web` SHA256：
`6c55e4af0850443406976bdd42a36949ff91609bf9775daf4fbafb04482a0879`。
17:34:26 仅重启 Manager 服务，CPU 仍为 24，前端和 Nginx 没有更换。
启动冷加载期间 API 暂不可用，之后恢复；新 Manager PID 为 1583011。

固定截止 **17:31:00**，校准前后对比：

| 查询 | 成交数 | 费后订单 NAV 变化 USDT | 校准前后账户结果 |
|---|---:|---:|---|
| trade04 策略锚点 1D | 23602 | 6357.475666721425 | summary、symbols、points 完全一致 |
| trade04 策略锚点 7D | 113485 | 9142.898456875339 | 同上 |
| trade04 账户锚点 1D | 23602 | 6357.475666721425 | 同上 |
| trade03 对照 1D | 31605 | 4528.687517184417 | 账户及策略均一致 |

各查询策略之和等于账户 NAV，差异小于 1e-6。
同一固定窗口结束时，页面未分配持仓名义金额由 41653.37865379543 降到
13.821457018258291 USDT；与校准瞬间的 8.09 不同，因后续成交和估值时点不同。
1D 的 768 个、7D 的 641 个共同校准前策略采样点值完全保留。
7D 另有 5 个采样时点集合差异，来自曲线抽稀，同一时点没有历史值变化。

四张原始起仓表和账户绑定哈希不变，其他 source 的 allocation 不变。
90 个其他受保护二进制进程、35 份配置不变；trade04 六角色在 Manager 发布期间
未再次重启，Redis PID/run ID 未变。Manager 预期更换，未计入未变进程。
最终 gateway、Viz、snapshot、Config 均 HTTP 200，`position_ready=true`，
Manager health `ok` 且无 refresh error。临时诊断会话均在 finally 删除并复核。

资金费率内存缓存随 Manager 重启冷加载，部分覆盖期间明确标记 loading/partial，
不会把缺失资金费造为零。最终 1D、7D、trade03 对照均退出 loading，覆盖原因为空；
资金费收入分别为 -224.78080279303828、-851.2006356930196、-225.42895893544036，
与维护前完全一致，逐策略资金费也一致。1D 比维护前多覆盖 6 个零金额结算，
7D/对照结算数不变。详见 `funding_reload.jsonl`、`funding_comparison.json`。

## 验证和边界

- Manager `cargo fmt --check`、`cargo check`、全量 `cargo test`、release 构建通过。
  419 库测试、6 校准工具测试、9 策略快照工具测试通过；原有 5 库测试及
  16 个依赖外部 PostgreSQL 的集成测试维持忽略。
- Exec `persist_common` 15、`persist_manager` 65 测试通过；导入器 release 构建通过，
  覆盖原子性、源身份、重复执行、冲突拒绝、原始手机身份和仅本批回滚。
- 这是已查实历史数据与当前归属的修复；未改造在线 Exec/Redis 的持久机制，
  不宣称已消除未来异常停机或交易所回报缺失的一切风险。
- 订单 NAV 仍不包含充值、提现、清算罚金等账户资金流水。保存的实际资金费与
  策略估计资金费保持独立。

本地完整证据目录：
`artifacts/jp_meta_trade04_unallocated_20261024`（初始冻结调查）、
`artifacts/jp_meta_trade04_apply_alloc_20261024`（25 笔与全历史）、
`artifacts/jp_meta_trade04_external_fills_20261024`（71 笔、最终校准、发布和验收）。

远端已将两次维护的 staging 证据归档到
`/home/ubuntu/crypto_cta_manager/diagnostics/trade04_repair_20261024/native25` 和
`external71`；已删除本次上传的两个原生导入器及两个只读诊断可执行文件，
旧 staging 路径不存在。完整 checkpoint、业务表备份和旧 Manager 回滚二进制继续保留。
本地大体积 JSON 证据使用 gzip 无损压缩，解压前 SHA 保存在 `compressed_evidence.json`。

后续操作者反馈今日仍有 100 多 U，详见
[prc 今日未分配盈亏复核](jp_meta_prc_residual_20261024.md)。19:08 另发布历史查询
首现时间索引修复；本文件所列 17:34 二进制与验收结果为当时记录，继续保留。
今日 -140.58 U 订单盈亏主要来自 17:20 校准前，不能将当前归属校准描述为历史重建。
