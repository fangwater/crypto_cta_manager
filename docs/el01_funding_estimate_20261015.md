# el01 Funding 估算发布 — 2026-10-15

el01 已发布 `web-releases/20261015T065429Z`。四个二进制及四个前端文件与
jp-meta 的 `20261015T063658Z` 发布逐字节一致，功能代码来自 `4a216a9`。
部署 checkout/manifest 记录 `0c7e864`，该后续提交仅增加 jp-meta 发布记录，
已确认应用代码与 `4a216a9` 一致。使用既有 NVMe release 产物，没有另建 checkout。

通过 `scripts/deploy_host.sh --target el01 --skip-build --manager-only` 发布，
只重启 Manager，PID 为 `1089257`。策略 PnL 包含估算 Funding，同时提供独立
金额及累计曲线；账户总体按采集开关选择实际或估算。trade09 保持实际采集开启，
其余启用账户保持关闭并使用估算。公开费率采用默认配置，主机 TOML 无需变更。

## 验证

14 个启用账户均检查账户和策略两种口径，共 28 组固定时间窗口。发布前后每组
完整订单 NAV 报告的规范化 SHA-256 一致，原实际 Funding 金额也一致。默认窗口
为最近一天，必要时按账户快照起点缩短。真实 source ID
`binance_exec_xy_lxy21_05` 保持原身份，没有重命名为已停用的 trade05。

28 组估算均完成加载，无不可用原因。逐策略资金费加总等于账户估算，费前/费后
合计等于原订单 PnL 加 Funding；独立资金费曲线从零起步，曲线终点与汇总一致。
实际账单始终单独保留，未同时叠加实际和估算。

四个二进制与 manifest 及 jp-meta 产物散列一致；四个前端文件通过 10051 网关
下载后的 SHA-256 与发布文件及 jp-meta 文件一致。所有 14 个账户的 Viz 首页、
snapshot 和 Config 共 42 条网关路径在发布前后均为 HTTP 200。

Manager health 为 `ok`、`last_refresh_error=null`。trade09 保留 674 笔实际
Funding 记录，重启后补齐已完成，最终 `status=ok`、无错误。目标历史索引先从
2,822,144 条增长至 3,637,248 条，最终处理 **3,819,438** 条，`ready=true`、
`loading=false`、无错误。最终状态保存于 `final_status.json`。
同期另查 jp-meta：索引也已完成，`ready=true`、`loading=false`，处理 4,744,591 条。

## 运行保护与清理

91 个受保护进程的 PID、启动标识和 CPU affinity 保持一致，收尾时再次核对通过。
其中包括交易、持久化、行情、Viz、Config、Nginx、Redis、PostgreSQL 和监控。
Manager 仍可使用原 CPU 0–7，监控二进制未替换。17 份配置文件散列不变，包括
14 份交易 TOML、Manager TOML、Nginx 配置和 Nginx unit。Manager unit 的有效
配置不变，Nginx 无 Manager 强依赖且未重启。

四张仓位快照表完整内容散列不变，所有 Funding 开关和轮询间隔不变。没有 schema
变更，也没有修改 Exec 状态、交易绑定或策略目标。验证用会话均在 finally 删除，
并再次查询确认不存在。

成功验证后，临时回滚目录中的八个精确文件记录散列后删除，确认目录、`.next`
文件和部署校验文件均不存在。正式 release 与既有历史 release 保留。

证据位于 `artifacts/funding_estimate_el01_20261015/`，包括发布日志、期望产物散列、
发布前后报告、状态、清理结果和验证脚本；没有保存会话 token 或数据库、交易凭据。
