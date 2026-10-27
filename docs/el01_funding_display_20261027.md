# el01 Funding 刷新闪烁修复

2026-10-27 操作员反馈账户页 Funding 和含 Funding 净值持续闪烁。
原先每次 NAV generation 更新，API 会隐藏上一轮 Funding，返回金额为空的
“计算中”，直到后台逐账户计算完成。浏览器 15 秒轮询会反复看到数值与 `--`。

运行提交 `6ba6577` 保留上一份有效 Funding，在后台新结果就绪后直接替换。
同一持仓锚点、同一类型的临时未知结果和计算失败也不覆盖已有金额；首次未知
仍保持未知，持仓锚点变化后的未知结果不能沿用旧锚点金额。`refreshing` 单独
描述后台更新，保留旧金额实际对应的时间戳和覆盖状态，鼠标提示显示统计截至
时间与更新状态。费用公式、每日保存、历史补齐、实际/估算选择规则均不变。

仅向 el01 发布，目录为
`/home/el01/crypto_cta_manager/web-releases/funding-display-20261027T062714Z`。
后端 SHA-256 为
`741bd80a5ccbc7dfcc27e3ba9e8223e674ce60e06740c7a11ca8dd1c68bdaca0`。
只替换 Manager 后端与前端并重启 Manager，最终 PID `4002395`，无自动重启，
CPU affinity 保持 `0-7`。主机 TOML、unit、数据库结构、交易服务和实际 Funding
采集设置均不变；发布前检查了已有 `restatements` 列。旧二进制、配置与前端
链接保存在主机 `backups/funding-display-20261027T062714Z/`。

验证：

- `cargo fmt --check`、`cargo check --locked`、聚焦回归测试、全部库测试
  （432 通过、5 跳过）、release 构建与 `npm run build` 通过。
- 经现有 Nginx 鉴权接口连续采样 132 秒、45 次，跨 3 个 NAV generation。
  14 个账户首次有值后没有一次退回空值；观察到 43 个旧金额在新 generation
  计算期间保留的实例，后续新结果正常替换。该检查验证 API 行为，未运行浏览器。
- 14 个账户共 28 份固定区间订单 NAV 报告与发布前完整哈希一致。
- 90 个受保护进程、16 份配置哈希、四张初始持仓快照表、归属校准表和每日权益
  表的行数与整表哈希、实际 Funding 开关与轮询间隔全部不变。
- Nginx 提供的 4 份前端文件 SHA-256 与发布清单一致。临时鉴权 session 和
  精确部署暂存目录已清理。

清单、前后基线及连续采样结果见 `artifacts/el01_funding_display_20261027/`。
