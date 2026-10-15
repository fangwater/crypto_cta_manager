# jp-meta Funding 估算发布 — 2026-10-15

代码提交 `4a216a90ab60ea2d3e254c059d99b1ea236b940f`，发布目录
`web-releases/20261015T063658Z`。运行 `scripts/deploy_host.sh --target jp-meta
--skip-build --manager-only`，使用当前 checkout 经 Cargo 确认的 release 产物及
重新构建的前端。只重启 `crypto-cta-manager-web.service`，PID 为 `4155551`。

## 展示口径

- 策略 PnL 包含整点持仓估算的 Funding；策略表另列 Funding 金额，并提供独立
  「策略 Funding」累计曲线及零参考线。
- 账户总体在实际采集开启时使用实际 Funding，关闭时使用估算，保留历史账单
  不改变这一选择。RX02、RX01、RX03 的采集开关继续开启，使用实际账单。
- 公开 USD-M 历史覆盖 USDT/USDC，默认六币、按需加载、30 天缓存、每 30 分钟
  刷新，固定 `limit=1000`。使用默认 `[funding_rates]`，无需修改现有主机 TOML。
- Binance 15 秒偏差仅保留在文档，页面不显示该提示。

## 验证

发布前后固定同一时间窗口，检查 trade03、trade04、RX02、RX01、RX03 的账户和
策略两种口径，共 10 组。默认选择最近一天，RX03 按其较晚的快照起点缩短窗口。
每组完整订单 NAV 报告的规范化 SHA-256 一致，原实际 Funding 金额也一致。

新增估算在 10 组查询中均完成加载，无不可用原因；每个策略的 Funding 加总等于
账户估算，费前/费后总体值均等于订单 PnL 加估算 Funding。独立 Funding 曲线零起点、
终点资金费及含资金费 PnL 均与汇总一致。trade03 实测包含 USDT 和 USDC，
trade03/trade04 使用估算，三个 RapidX 账户总体使用实际账单，策略始终使用估算。

所有四个已部署二进制与 manifest 匹配，四个前端文件通过实际 4191 网关下载后的
SHA-256 匹配发布文件；五个账户的 Viz 首页、snapshot 和 Config 共 15 条路径返回
HTTP 200。Manager health 为 `ok`、`last_refresh_error=null`；三个 RapidX 实际
Funding 采集均为 `ok` 且无错误。

发布后的理论目标历史索引正常后台重建：两次检查的已处理消息数从 573,696 增长到
1,150,464，`loading=true`、`last_error=null`。这属于重启后的现有冷启动流程，
事实 NAV、策略估算 Funding 和实际 Funding 已可查询；未将索引未完成标为已完成。

## 运行保护与清理

187 个受保护进程的 PID、启动标识和 CPU affinity 全部一致，包括交易、行情、
persist_manager、Viz、Config、Nginx 和独立监控。Manager 保持 CPU 24；监控二进制
散列不变。29 份配置（含 Manager TOML、25 份交易 TOML、Nginx 路由）散列不变；
systemd unit 的有效配置与发布前一致，仅规范化文件格式。四张仓位快照表完整内容
散列一致，所有账户 Funding 开关与采集间隔不变。没有数据库 schema 变更。

校验会话每次都在 finally 删除，并确认数据库中已不存在。部署暂存文件由脚本清理；
成功验证后，将临时回滚目录中的七个明确文件逐一记录 SHA-256 并删除，再确认目录
和所有本次 `.next`/校验文件不存在。正式 release 和既有历史 release 保留。

证据及只读验证脚本位于 `artifacts/funding_estimate_jp_meta_20261015/`：
`before.json`、`after.json`、`verification.json`、`status.json`、`cleanup.json` 和
`deploy.log`。没有保存会话 token、数据库或交易凭据。
