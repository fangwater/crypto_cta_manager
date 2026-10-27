# jp-meta FR 缺价余额不再遮蔽已定价仓位

2026-10-27 修复 Gate FR 首页多空持仓、杠杆与轧差全部为空的问题。
`gate_fr_arb01` 快照中 SLVON、TSLAX 数量非零而名义金额为零；
`gate_fr_arb02` 同时存在 CRCLX、FTM、MATIC、SLVON、TSLAX 缺价余额。
旧的 NAV 汇总在任意一条缺价腿上返回错误，导致账户全部仓位字段为空。

新实现保留已定价腿的小计，使用相同小计计算杠杆与轧差，独立导出
`position_unpriced_assets`。CTA 首页对应数字带 `*`，账户旁可展开
“部分估值”查看缺价资产。不以币数量推断金额，不将缺价资产估为零；
一侧完全缺价时该侧仍为空。完整账户继续使用原值。已定价部分超出杠杆
阈值仍可告警，但存在缺价余额时不能由小计断言没有超限。
同一规则也恢复 Bybit FR 已定价部分，EUR、HFT 明确列为未计入资产。

历史日期不借用当前缺价清单；过期仓位不继续显示。权益、保证金、
FR 实现盈亏、FIFO、费用、资金费、利息、交易和历史快照逻辑均未改动。
没有调整交易持仓，没有执行账户余额清理，也没有增加行情 REST 请求。

发布 `fr-positions-20261027T082404Z`：

- NAV 源码 `f40b6ec` 已推送并同步远端 master。当地构建的主服务及七项
  部署清单中的二进制来自 NVMe release 目录；所有 migration 文件哈希
  与远端发布前一致。
- CTA 后端及前端代码 `ee06d8e`。前端位于
  `/home/ubuntu/crypto_cta_manager/web-releases/fr-positions-20261027T082404Z`。
- 仅重启 NAV Manager 和 CTA Manager。最终 PID 分别为 `2977395`、
  `2977372`，自动重启次数为零，CTA CPU affinity 保持 `24`。
- 原二进制在切换前建立可回滚备份，待全部网关及状态验证通过后精确清理
  本次暂存目录和临时备份。原前端 release 保留，新 release 也保留旧 assets。

验证：

- 两仓格式检查、Cargo check、release 构建通过；NAV FR overview 的
  6 项测试通过，CTA 全部库测试 433 通过、5 跳过。
- CTA 前端生产构建、3 项 workspace 测试、lint 通过；lint 仍有两项
  已有未使用声明警告，构建仍有既有包体大小提示。
- 真实 Nginx 接口验证两个 Gate 账户仓位、杠杆、轧差为有限数字，
  轧差等于多头减空头、杠杆等于对应持仓除以权益，缺价清单与快照一致。
  所有 10 个 FR 账户身份保留，Gate arb03 缺价清单为空。
- 历史日期仓位保持未知，且不沿用当前缺价清单。
- `/nav/`、`/nav-api/health`、`/manager/` 与所引用 JS/CSS 均返回 200；
  CTA 静态资源 SHA-256 与构建清单一致。本次未运行浏览器渲染验证。
- 156 个受保护进程身份及 affinity、32 份配置哈希、四张初始持仓快照表、
  21,095 条归属快照、182 条每日权益审计记录和 funding 设置均保持一致。
  核对排除此次授权重启的 NAV 主进程及其可自行退出的 sync_history 子进程。
- 临时诊断 session 已删除。CTA 首次全量历史加载期间暂时返回 502，
  自动等待完成后通过全部接口核验，无崩溃或自动重启。

脱敏基线、接口结果、产物清单、发布及比较脚本见
`artifacts/jp_meta_fr_positions_20261027/`。
