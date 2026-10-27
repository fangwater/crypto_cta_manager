# jp-meta 本地 CTA 可见范围与临时统计排除

按 2026-10-27 操作员要求，以下来源仅允许 `lgq` 在 jp-meta Manager 查看，
`prc` 即使是管理员也不能访问：

| Source ID | 账户别名 | Exec 入口 |
| --- | --- | --- |
| binance_exec_trade06 | lgq_cta_RX01 | /exec_trade06/ |
| binance_exec_trade07 | lgq_cta_RX03 | /exec_trade07/ |
| binance_exec_trade08 | lgq_ltp_81857 | /exec_trade08/ |
| binance_exec_rx04_5m05 | RX04 5min | /exec_trade05/ |

主机 `dashboard.source_viewers` 为按 source ID 配置的用户名白名单，是数据库
授权及管理员角色的共同上限。`lgq` 原有 configure 授权保持有效。账户列表、
权益总览、净值 API、symbol 发现、授权元数据与 Nginx 的 Exec 验权均遵守
此上限；用户授权替换仅更新调用者可管理的来源，保留隐藏来源原有授权。
普通用户的策略授权及机器发布规则保持原状，浏览器发布响应过滤隐藏账户。

排查总览时，`lgq` 的 21 个账户中 20 个已有实时权益；Gate FR05 缺失权益，
Gate FR04/05 缺少当日盈利，因完整性规则导致“总权益”和“今日总盈利”为空。
这与是否到次日无关。按随后指令，在 `dashboard.nav_overview` 配置
`excluded_source_ids = ["gate_fr_arb04", "gate_fr_arb05"]`，暂时从最新及历史
总览排除这两个账户。上游采集、账户启用状态和历史记录不变；恢复时删除这
两项排除即可。剩余账户的缺失、过期检查仍然有效，不用零填充未知值。

本次只部署 jp-meta Manager，保留既有前端。更新现有 Manager 工具二进制，
使它们也能读取增加了权限配置的 TOML；只重启 `crypto-cta-manager-web.service`，
不启动或重启 monitor、交易、Viz、Config、Nginx、NAV Manager 或 SG 服务。

验证和发布记录见 `artifacts/jp_meta_lgq_visibility_20261027/`。

发布完成：`lgq-visibility-20261027T094457Z`，功能提交 `cc5c9b2`。
`cargo fmt --check`、`cargo check`、全量 `cargo test` 和 release 构建通过；
438 项库测试、18 项二进制测试通过。另在隔离 PostgreSQL 中运行授权替换
回归，确认隐藏授权保留、越界授权拒绝、数据库 configure 授权不能突破
主机可见范围；测试集群和 socket 已清理。

真实 4191 网关双用户验证通过：

- `lgq` 看到 6 个本地 CTA 与 13 个外部账户，共 19 个；所有账户权益和
  当日盈利完整，两个汇总可计算。四个私有 CTA 仍有 configure 权限。
- `prc` 只看到本地 trade01–04，无私有来源或外部账户元数据；四个来源的
  账户详情、授权、symbol、timeline、Exec snapshot 和 Config 请求均被
  403 拒绝，尝试为自己添加私有账户授权也被 403 拒绝。
- 最新及历史总览都排除 Gate FR04/05；匿名 symbol 发现不含私有来源，
  显式匿名查询被拒绝。授权列表、虚拟账户跟随、归档也不暴露私有账户。
- 133 个保护进程身份及 affinity、31 份非 Manager 配置、10 张快照/目录
  表与所有账户授权哈希一致。前端目录不变，Manager CPU 仍为 24，
  `MainPID=3163299`，`NRestarts=0`。

只重启了 jp-meta `cta_web`。临时诊断 session 全部删除，核验后清理本次
精确暂存目录和临时二进制/配置备份；此前前端 release 保留。
