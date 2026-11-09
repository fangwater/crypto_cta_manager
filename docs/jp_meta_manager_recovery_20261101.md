# jp-meta Manager 启动失败与恢复（2026-11-01）

## 故障与原因

16:23:52 UTC，现场开始重启 Manager；16:24:11 启动的新程序为
`fecb8a4384171967cb9d69ab5af8b34fb71523ec7a9011b74f144386a13d8a73`，
与此前 el01 的 daily-FIFO 程序相同。程序初始加载 NAV 时报告
`restated allocation original fingerprint changed`，随后退出并由 systemd
重新启动。初始 NAV 构建发生在 HTTP 监听之前，因此静态前端返回 200，
Manager API 返回 502。磁盘、主机内存和 PostgreSQL 不是此次退出原因。

`src/nav/allocation.rs` 中的 `positions_fingerprint` 对原归属记录的类型化
positions JSON 计算 SHA-256。后补历史归属修订的 `restatements` 保存这个
指纹，以保证只修改审核过的原记录的有效 before 数量，保留原始行、after
目标和参考价格。历史修订支持见提交 `d8bd28b` 及
`jp_meta_prc_historical_ownership_20261024.md`。这是内部完整性约束；运行者
无需提供指纹、密钥或额外文件。

daily-FIFO 程序全局启用了 `serde_json/float_roundtrip`。旧指纹是在默认
解析器下生成的，启用该 feature 改变了部分十进制数到 f64 的末位舍入。
对相同 JSON 重算类型化 JSON 指纹时，字节发生变化。

对 jp-meta 只读导出的 21,095 条归属记录离线复现了生产算法，覆盖全部
528 项修订：

| 解析设置 | 通过 | 不匹配 |
| --- | ---: | ---: |
| 旧程序默认解析 | 528 | 0 |
| 新程序 float_roundtrip | 451 | 77 |

首个失败目标是 trade02 的 `1792849569342000` 归属记录，共 124 条腿。
其中三个数值字段发生末位变化，例如 ETCUSDT 的 after 数量从
`4.768371585604782e-8` 变为 `4.768371585604781e-8`，差约 `6.62e-24`。
原指纹为 `24c655e84cb6675406c86b669ae4fa8666731ba24efc0bd3ec2857fd0b8190da`，
新解析方式计算为
`3f2a556cfa1a58d261c809472a4f11b9daf570c0233167717b830e2a052ca36c`。
这是软件与既有审计数据的兼容问题，不能据此认定缺数据或改写保存的指纹。

## 恢复操作

确认目标为 `jp-meta` / `ip-172-31-35-228`，Manager 根目录为
`/home/ubuntu/crypto_cta_manager`。先保存交易等受保护进程、配置及不可变
快照摘要，并硬链接保留故障程序，再从本次发布已有备份
`backups/deploy_20261101T162350Z/cta_web` 恢复原程序：

`ffb015bcbb4cafcaa4e4a417df1e39cd6976aad1b035597232daa000caad709f`。

16:40:12 UTC 只重启 `crypto-cta-manager-web.service`，PID 为 `2721895`，
继续绑定 CPU 24。16:43:19 初始报表完成，HTTP 后端恢复。
保留 Manager 自有的 daily-FIFO CF；原程序不使用该派生缓存。
没有删除历史数据、修改归属记录、重算保存的指纹或跳过完整性检查。
没有发布手工交易目标、改动交易绑定或操作其他主机。

故障程序及操作证据位于主机的
`diagnostics/manager-recovery-20261101/`。本地脚本和核验摘要位于
`artifacts/jp_meta_manager_recovery_20261101/`；原始只读业务数据导出保存在
本机仓库外的受限 incident 目录，不放入版本控制。

## 已完成的验证

- 外网前端返回 200，未登录 API 返回 401，消除原来的 502。
- prc 与 lgq 的登录态健康均为 `ok`，周期刷新成功，`last_refresh_error=null`。
- 8 个启用 CTA 来源分别查询账户、策略两类净值，共 16 次全部返回 200。
- 理论目标索引完成 7,444,156 次消息处理，`ready=true`、`loading=false`、
  `last_error=null`。trade02、trade04 的一小时理论曲线各返回 61 个点，
  缺失价格为零，没有 loading 或 unavailable 原因。
- prc 的四个账户总览完整，lgq 的私有来源可读；prc 对私有来源仍返回 403。
- 92 个受保护进程的 PID/启动标识、31 份配置摘要、5 张不可变持仓及归属表
  摘要、BNB/BFUSD 设置及前端指向全部一致。
- 临时诊断登录会话均在 finally 中删除，服务 `NRestarts=0`。

主机未部署新的兼容性补丁。本次通过恢复已验证的原程序恢复服务；
`fecb8a...` daily-FIFO 程序不能再次原样部署到 jp-meta。后续变更数值解析或
审计序列化之前，需要使用真实保存的修订指纹做离线核验，不能直接放宽校验
或重写不可变记录来通过启动。

附带观测：首次核验中 BNB 状态 GET 等待原有自动任务的状态锁超过 55 秒；
该辅助请求的会话已清理，未提交任何人工资金操作。lgq 的外部
`okex_mm_alpha` 曾返回未知权益；这与 Manager 启动失败分开记录，不能把未知
权益计为零，也不据此声称外部所有上游已恢复。

## 后续：HTTP 推送及共享 Redis 故障

操作员随后报告仓位 HTTP 推送仍失败。初次恢复验证主要覆盖读取接口，未覆盖
推送和 Redis 写入；前述 Manager 恢复并未解决共享 Redis 的独立故障。

后续统一通过 `jp-meta-elvpn` 操作（10.61.20.5:10022，与公网别名访问同一
`ip-172-31-35-228`、同一 Manager PID）。确认 Redis PID656561 从凌晨起
持续占满一个核，PING/GET 不返回。perf 栈在 clientsCron 的内存分配路径内
循环。日志记录外部复制请求，AOF 中有两次 FLUSHALL，基础 AOF 中保存了
名为 `config_bypass` 的函数库。这些操作来源尚未查清。

先完整保留原配置、AOF/RDB、日志和进程元数据于主机
`diagnostics/manager-recovery-20261101/redis.before/`，约 66.9 MB；两份 AOF
校验通过。离线按 AOF 完整重放的最终状态只有 19 个键，表明当时备份的是
受损现场，不能声称它包含丢失前的全部数据。

17:00:41 UTC，从原有未编辑的 AOF 恢复共享 Redis，新 PID2769589。主线程
不能处理 SIGTERM，停服务后仅强制结束该挂起进程。没有清空在线库或替换
AOF 内容。合并既有 default ACL，限制 ACL/CONFIG/REPLICAOF/SLAVEOF/
MODULE/FUNCTION/FCALL/FLUSHALL/FLUSHDB/DEBUG/SHUTDOWN 管理命令；GET/SET、
事务和业务 EVAL 保持。CONFIG GET 和 FCALL 的负向核验返回 NOPERM。
这些限制不是完整的入侵来源调查或安全事件结案。

恢复后的原客户端自行重连。短时 Binance 429 在下一分钟退去，17:01 后
rbf_big、rbf_small、rbf_mid_1min 均重新成功发布，17:05 后抽取的 420 次
绑定发布中 340 次属于 rbf，未出现绑定发布失败。实际网关推送恢复 HTTP200。

## RX04 5min 执行恢复

`/exec_trade05/` 当前对应 `binance_exec_rx04_5m05`，不是已停用的旧
`binance_exec_trade05`。Redis 阻塞前的快照停在凌晨；恢复 Redis 后快照
变为实时，但其因子配置和未变化的运行策略仍缺失。

- 通过 RX04 现有 19188 配置服务，用当前主机
  `config/cta_perp_symbol_factors.json` 补回缺失的因子键，15 币的源配置
  文件哈希及 enabled=true 均保持。17:05 新五分钟帧重新发布并开始下单。
- 用 Manager 官方 target-preview/publish 接口补回 28 个缺失绑定，35 个
  运行策略完整。每次发布在 Manager 配置锁内读取当前目录，没有提交历史
  模板或手工数量，也没有改变份额和算法。
- 旧未决订单状态仍使 14 行持仓未分配。先验证 35 个策略账本与账户快照
  数量相符，再仅通过环境原脚本重启 RX04 exec-pre-trade，按原流程撤单。
- 首次启动因同样被清掉的风险参数 hash 缺失而拒绝启动。找到 10 月 23 日
  已验收的 `risk_verified.json`，按确切记录补回五项：挂单上限10、买卖各10、
  400单/分钟、300单/10秒。未使用同步脚本中不同的200单/10秒默认值。
- 再次通过原环境脚本启动成功，PID2800505，现有程序、env.sh、交易 TOML
  哈希保持；实际重新读取当前 env 的1000ms配置刷新间隔。
- 17:12 的实时快照 `position_ready=true`、未分配行0、活动订单行9。
  剩余交易所最小单量和正常 Grid 执行状态不通过改量或改算法消除。

此次 Redis 和 RX04 恢复过程中，Manager PID2721895 保持；未重启其他账户的
交易进程，也未重启 RX04 的 trade_engine、account_monitor、persist_manager、
Viz、Config。最初的92个受保护进程中，只有指定的旧 RX04 executor 被替换。

## 按操作员最新优先级恢复 symbol 列表与 FR 参数

操作员指出 Redis 全量内容仍缺失，随后明确优先恢复 `/delist/` 所用全部
symbol 列表及全部 FR 策略参数。找到10月24日完整 RDB（1,777,378,252字节，
619键，校验通过），在无 TCP 监听、禁用持久化的隔离实例中读取；没有把
旧库整体覆盖到在线库。首次对照有488个旧备份键在当前库中缺失，旧执行
目标和持仓不能作为当前事实直接恢复。完整范围见
`jp_meta_redis_restore_plan_20261101.md`。

17:19–17:21 UTC，按本次明确范围，仅创建仍然缺失的键：

- 89个 symbol 列表，包括各账户和公共 FR/intra/cross/CTA/MM 列表。
- 28个 FR 账户参数键，包括风控、资金费阈值和已保存的逐币覆盖。
- 13个 FR 共享参数键，包括四个场所组合的策略参数、价差阈值和映射。
- 4个 FR rolling 参数键。

共134键。RESTORE 不带 REPLACE，并保留并发创建的现有值。9个在线 JP FR
账户的 symbol-lists、risk-params、strategy-params、funding-thresholds、
rolling-params 和 spread-thresholds 共54次HTTP读取均200；每个账户读取到
19项风险参数和25项共享策略参数。89个已回填列表及原130键复核没有缺失。

恢复前的在线状态另存 `redis.current_before_merge.rdb`，SHA-256为
`cae8e413fb24c171da6f9eb62d11caa2ec5079e371fc43160354ca15be0562d1`。
原始完整RDB、受损现场备份及每键恢复清单均保留。尚未找到10月24日之后的
正常全量备份；其他Redis内容并未全量回填，不能声称已恢复故障前全部状态。
