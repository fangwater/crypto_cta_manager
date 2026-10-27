# jp-meta MM / SG Exec 总览接入与 lgq 可见范围

按 2026-10-27 操作员要求，在 jp-meta 总览加入 `okex_mm_alpha`、
`bybit_mm_alpha` 和 SG `bybit_exec_trade01`，与现有 FR 一同仅向 `lgq` 展示。
`prc` 当前为管理员，因此原来的“管理员可见全部外部账户”逻辑不足以满足
要求。本次 provider 的 `viewer_usernames = ["lgq"]` 同时作为明确授权和
可见上限，管理员也无法绕过。

数据路径：

- NAV 的既有只读汇总接口增加两个指定 MM，保留其既有金额 FIFO、实际
  手续费和资金费。每日盈利为已实现净收益；权益与回撤仍包含浮盈亏。
  实时仓位来自各自 Viz，Bybit MM 使用 SG 网关。历史计算单独后台执行，
  并发数为一，避免大历史重放阻塞其他账户实时事实更新。
- SG Manager 通过独立 Bearer token 导出已有账户总览缓存，配置明确限定
  `bybit_exec_trade01`。jp-meta 新增 `sg-cta` provider，只读 HTTP 拉取。
  身份为 `external:sg-cta:bybit_exec_trade01`，详情进入 SG Manager。
  无跨主机 RocksDB 路径或持久运行状态共享，没有在 jp-meta 创建 SG Exec 来源。
- 页面区分 CTA / FR / MM，并保留已定价仓位小计与缺价资产提示。
  外部账户的过期与缺失值仍阻止不完整汇总被显示为完整总额。

可见范围覆盖最新及历史账户行、汇总输入、外部日期、provider 状态和权限
列表；`prc` 对这两个 provider 的权限修改请求也返回 403。其他本地 CTA
授权按原规则工作。这是 jp-meta 的聚合访问范围，不改变 NAV 或 SG 自己的
登录和原有账户权限。主机 token 仅保存在权限 0600 的环境文件中，通过
Manager user-service drop-in 加载，不下发浏览器或保存到仓库。

发布 `mm-exec-overview-20261027T090404Z`：CTA 代码 `68ad085`，
NAV 代码 `2395d73`。所有二进制均在本机 NVMe target 构建；NAV 提交已推送，
远端 master 快进同步，七个发布清单二进制一起更新，migration 文件哈希一致。
jp-meta 更新 CTA 后端/前端及 NAV 后端；SG 只更新 Manager 后端，保留原前端。

SG 旧版本缺少新版必需的目录字段和归属审计表。切换前在主机上创建完整
数据库备份，再显式运行 `apply_grid_order_schema.sql`、
`apply_order_strategy_ownership.sql` 和 `apply_allocation_schema.sql`。
归属表仍为空；现有订单模板增加默认 Grid 参数和空 creator，原有字段逐一
哈希一致。没有执行启动 DDL、删除数据或改写持仓快照。该数据库维护备份
保留在 SG 的 `maintenance-backups/mm-exec-overview-20261027T090404Z.dump`。

验证：

- NAV 212 项库测试全部通过；CTA 435 项通过、5 项显式跳过。格式检查、
  Cargo check、release 构建、前端生产构建与 4 项 workspace 测试通过。
  lint 只有两个既有未使用声明警告。本次未运行浏览器渲染验证。
- 以 `lgq` 的临时 session 读取真实网关，两个 MM 与 SG Bybit Exec 的
  权益、多空持仓、杠杆、轧差、保证金和当日盈利均有数据。
- 当前目录共有 12 个 FR，因此 `lgq` 共看到 15 个外部账户。核验期间目录
  还包含 Gate FR04、FR05；FR05 上游无有效权益，明确标为 unavailable。
  最终验证按实时启用目录确认完整身份集合，不固定 FR 数量。
- `prc` 最新、历史总览均无外部账户及 provider 元数据，外部权限列表为空。
  尝试为自己授权 MM、Bybit Exec 均被 403 拒绝，查看授权表哈希不变。
- SG 导出仅含指定 Bybit Exec；匿名及仅带 browser session 的请求均被
  401 拒绝。详情链接和 jp-meta 前端资源返回 200，资源哈希与构建清单一致。
- SG 两个固定区间 `timeline` / `account-timeline` 的整体与所有保留策略
  数值完全一致。新版仅省略旧报告中 symbol、仓位、成交、费用及每个净值点
  都为零的 `system_position_close` 系列；验证逐个检查其所有数值为零。
- SG 25 个、jp-meta 132 个受保护进程身份与 affinity 保持不变；分别
  5 / 31 份非本次计划修改的配置哈希一致。双方初始快照、既有归属记录、
  每日权益记录、策略绑定、跟随绑定和原订单模板参数均保持一致。
- Manager affinity 分别保持 SG `0-7`、jp-meta `24`，无自动重启。
  临时诊断 session 均删除。完成验证后精确清理上传暂存及临时二进制备份，
  保留主机数据库维护备份和原前端 release。

发布清单、脱敏前后状态、正反权限测试和比较脚本见
`artifacts/jp_meta_mm_exec_overview_20261027/`。`*-verified.jsonl` 为最终结果，
早期验证捕获了目录扩容与零系列省略后，已据实际证据调整比较规则。
