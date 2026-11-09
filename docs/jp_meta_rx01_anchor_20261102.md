# RX01 周五起点与分钟价格核验

最后更新：2026-11-02 UTC

目标为 `jp-meta-elvpn` 的 `binance_exec_trade06`（lgq_cta_RX01）。
Manager 位于 `/home/ubuntu/crypto_cta_manager`。本次未发布应用程序，
未改动交易目标、交易配置或 Exec RocksDB。

## 已完成

- 在 PostgreSQL `crypto_cta_manager` 的 `cta_position_snapshots` 追加
  `snapshot_ts_us=1793354400000000`，即上海时间 2026-10-30 18:00。
  对应 `cta_position_snapshot_entries` 追加 55 项现货余量，venue_code=0。
  数量逐项核对既有 `restore_audit_20261030/account_plan.json`，参考价采用
  Binance 17:59–18:00 完整分钟收盘价，起点参考名义价值约 3.595131 USDT。
  原 3 个账户快照、70 条账户明细及全部策略快照保留，未添加策略归属。
- 逐分钟检查至上海时间 2026-11-02 16:32，覆盖 55 项现货以及后续成交
  涉及的合计 93 个市场，每个市场 4,233 根分钟 K 线。诊断副本使用
  Manager 已配置专用出口及现有 KlineStore 补查公共数据，缺失为零。
- 新快照生效后，账户与策略接口均返回 HTTP 200、284 个严格分钟估值点。
  两种模式与独立只读回放一致：起点为零，2,688 次成交，末点费后订单 NAV
  为 -127.65458743363294 USDT，与汇总一致。该数值不包含资金费。
- 再次直接只读扫描生产 Manager 主缓存，55 项现货的全部 4,233 根
  分钟价格齐全，缺口为零。不存在用旧成交价填补分钟缺口的情况。
- 继续查询至上海时间 16:49，账户和策略接口均返回 HTTP 200、285 个点，
  2,716 次成交，费后订单 NAV 为 -127.32637534110762 USDT，两个接口一致。
  生产主缓存的 55 项现货各 4,250 根分钟价格亦逐分钟齐全。
- 核验期间 Manager PID612135、CPU24、NRestarts=0；受保护交易进程、
  配置、其他账户快照及所有策略/归属快照摘要保持一致。
  最终对照覆盖 85 个受保护进程和 30 份配置/程序摘要。

## 日权益修正和旧记录清理

Manager 当日已经成功生成日权益，后台任务和内存缓存不会因新快照自动
再算一次，因此快照生效后需要单独修正派生记录并刷新内存缓存。

按操作员确认，已更新 `cta_account_equity_daily` 中仅 RX01 的三条记录：

| 上海结算日 | 修正后开盘权益（USDT） |
| --- | ---: |
| 2026-10-31 | 19040.66972126592 |
| 2026-11-01 | 18975.4679380957 |
| 2026-11-02 | 18956.55507982102 |

今日旧值为 18954.734434833998，新值相差 +1.82064498702 USDT。
修正使用 Manager 自有不可变权益观测 `equity_at_ms=1793607457390`，
权益 18915.338852857403，匹配同一时刻的严格分钟估值净值和完整公开
资金费估算。资金费为 0.13467390602623178 USDT，loading=false，
unavailable_reason=null；没有把缺失资金费当零。该观测位于当日，
在所有所需资金费均已完成覆盖的区间内。

三条记录已通过生产 `account_overview::daily::reconstruct` 独立复算，
结果逐项相等。操作员随后同意更新，并明确询问清理更早记录；在说明仅删除
旧起点派生日权益后，09:06:52 UTC 以一个事务完成三条更新和 11 条删除。
删除范围仅 RX01 的 2026-10-20 至 2026-10-30 结算日，现存三条从
2026-10-31 开始。其他账户日权益摘要保持一致。完整 14 行旧记录已额外
备份，精确回滚 SQL 可恢复更新前数值和已删行。
未变更日写入调度。交易所观测和不可变持仓/归属快照均保留。

09:07:45 UTC 仅重启 `crypto-cta-manager-web.service`，新 PID828467，
仍为 CPU24、NRestarts=0，程序 SHA-256 仍为
`ffb015bcbb4cafcaa4e4a417df1e39cd6976aad1b035597232daa000caad709f`。
未重启交易进程，未替换 Manager 程序或改动配置。

09:10:56 UTC 完成重启后验收：Manager 健康为 `ok`、刷新错误为空，
工作台今日开盘权益实际读到 18956.55507982102，历史起点为上海时间
2026-10-31 00:00。实时权益 18929.299529683503，日收益
-27.255550137517275，逐项满足实时权益减今日开盘权益。
账户和策略固定区间净值均 HTTP200，284 点及汇总与重启前严格回放一致。
85 个受保护进程、30 份配置/程序摘要及 5 张 RX01 快照/归属表保持一致；
仅 Manager PID 变更。prc 直接访问该私有来源仍为 403，诊断会话已删除。

## 证据与回滚

主机审计目录：
`/home/ubuntu/crypto_cta_manager/diagnostics/rx01_minute_audit_20261102/`。
`database.before.json` 保存原 6 张相关表的 RX01 行；
`protected.before.json` 保存进程及配置摘要；`preview.json` 为严格回放；
`primary.coverage.json` 为生产主缓存核验；`online3.*` 为新快照生效后的
线上验证；`daily.candidate.json` 为复算结果。
`database.before_daily_apply.json` 为执行前完整 RX01 相关表备份；
`daily.prune.reviewed.sql.json`、`daily.prune.rollback.sql.json` 为实际执行
SQL 和精确回滚；`daily.prune.applied.json`、`manager.restart.json` 为回执。
SQL 带逐行旧值检查、来源和日期限制、更新/删除行数检查及事务锁。
`daily.final.overview.json`、`daily.final.health.json`、`daily.final.*timeline.json`
和 `protected.after_daily_apply.json` 保存最终验收结果。

本地复核代码和结果位于 `artifacts/rx01_anchor_20261102/`，没有提交无关
工作区改动。数据库凭据、API 密钥和诊断会话令牌未输出或复制；临时登录
会话已在 finally 中删除。
独立诊断 K 线副本、secondary 元数据和本次上传的三份诊断可执行文件均已
删除并确认不存在；既有诊断程序、原始备份、审核 SQL 及回执保留。
