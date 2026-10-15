# jp-meta RX01 实时权益恢复 — 2026-10-15

`binance_exec_trade06 / lgq_cta_RX01` 的 Manager 工作台返回空权益；其他六个
账户正常。Manager 已订阅正确的 RapidX IPC，问题位于账户监控的账单恢复。

RapidX REST 对 7 条 `DEDUCT_INTEREST` 账单重复返回相同身份、时间、金额和
债务字段，但 `beforeAvailable` 与 `afterAvailable` 同时相差 1e-20 USDT。
原来的严格字符串比较报 `conflicting duplicate statement`，使账户监控反复
重连、`recovery_ready=false`，无法发布可用的实时权益。账单游标停在
`1791981333633` ms；该故障早于本日 Manager 成交/funding 替换。

修复位于独立 Exec 源码 `/home/fanghaizhou/mkt_signal` 的
`crates/trade_engine/src/ltp_journal.rs`。仅当全部其他字段原样一致、两个可用
余额的偏移完全相同且绝对偏移不超过 1e-18 时，才按精确十进制接受复查结果。
原账单及其 ACK 保持原值，复查原文另存 `statement_balance_reobservation`。
金额、债务、身份、时间、余额变化量、超限偏移和科学计数法比较仍拒绝。
没有修改 IPC、persist_manager、仓位快照或 Manager 源码。

本地 `arbmm` 与 `origin/arbmm` 核对同步，基于 `133bf23f` 构建；仅上传并替换
RX01 的 `rapidx_account_monitor`，SHA-256 为
`d2f560ee4e51d229508376a6229d0343cc1a34af898b53e60ac76bdac4228949`。
`cargo fmt --check`、363 项 trade_engine 库测试和 release 构建通过。

03:46:52 UTC 经环境本地启动脚本启动新 PID `2309899`，旧 PID `3046353`；
绑核保持 CPU0-5。最初设置的 50 秒总超时不足以完成 supervisor 对全部登记
进程的多次加载，8 秒单次限制也不足；取消这些尝试后，使用原环境脚本及
180 秒总限正常完成。没有修改 supervisor 或其他服务配置。

验收确认：

- Manager PID `1850064` 及其他 190 个受保护进程的 PID/start time/绑核保持。
- 33 份配置、前端和 Manager 二进制、4 张初始仓位快照表校验一致。
- 原始 journal 的备份前缀和 542 条已有账单原值保持；账单总数追至 858，
  7 条尾差复查证据保存，成交/账单待持久化数均为 0，恢复就绪。
- 新账户监控日志无 WARN/ERROR；Manager 七个权益 feed 均为 `ok`。
  两次健康采样的交换时间相隔 107,910 ms，权益持续更新。
- Chromium 以 lgq 身份检查 1440×1000 与 390×1100 工作台，均显示
  `19,436.24 USDT`，与当次 dashboard API 四舍五入到分一致；无 JS 错误、
  失败响应或横向溢出。这是验收时的动态数值。

构建清单、进程/配置/快照校验、账单冲突和复查摘要
保存在 `artifacts/jp_meta_rx01_equity_20261015/`。补丁、原始 API 响应、新旧
二进制、日志及 journal 压缩备份已移至[仓库外本地归档](../artifacts/LOCAL_ARCHIVE_20261015.md)，保留逐文件
SHA-256 清单。
验证会话和浏览器已清理；远端临时备份在归档核验后按精确路径清理。
