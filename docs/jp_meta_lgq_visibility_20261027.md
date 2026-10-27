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
