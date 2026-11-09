# NAV 空历史策略筛选（2026-11-06）

jp-meta 发布：`nav_empty_strategies_20261106T062242Z`，仅原子切换 Manager 静态前端。

`cta-prep02-bn-1min-pos`、`cta-prep02-bn-5min-pos` 和
`cta-prep02-bn-15min-pos` 已无当前策略目录记录或账户绑定，但历史成交会使
NAV API 继续返回这些名称。最近一天里三条曲线的成交、盈亏、持仓和资金费
均为零，因此前端现在在策略选择、曲线和策略盈亏表中隐藏这些全零项。

筛选按所选区间的数据判断，不硬编码名称。任意成交、盈亏样本、持仓或估算
资金费都会保留原策略，包括很小的资金费、期末相抵但中途有变化的资金费和
净敞口为零的对向持仓。缺失或不完整记录保留显示。切回有贡献的历史区间时
原策略仍会显示；切换区间后的勾选操作只计算当前可见策略。

数据处理在 `frontend/src/lib/navStrategies.ts`，由 NAV 页面调用。API 响应、
FIFO、归属、账户汇总和原始历史记录不变。

验证：58 项前端测试、`npm run build` 和 `git diff --check` 通过。
修改前的完整前端构建与线上 index 哈希完全一致，确认只发布本次 NAV 变更。
真实 Nginx 网关上的 Chromium 桌面 1440×1000、手机 390×1100 验证通过；
旧名未出现在表格和策略选择中，Funding 视图正常，无页面错误、配置写入或
手机横向溢出。固定查询在末尾五分钟之外的账户、币种和策略曲线样本完全
一致；近实时端点允许后续收取的成交正常补入。

Manager PID `3952558`、102 个受保护进程身份、API 二进制和配置摘要保持
不变，未重启任何服务。静态文件哈希与网关返回内容一致。临时诊断会话已
逐一删除，两个具名上传目录及本地上传归档已清理，未保留截图或浏览器配置。

操作证据：`artifacts/nav_empty_strategies_20261106/`（Git 忽略）。
远端回滚记录：
`/home/ubuntu/crypto_cta_manager/maintenance-backups/nav_empty_strategies_20261106T062242Z/`。
原始静态发布 `web-releases/rapidx_margin_display_20261103T152300Z` 和本次两次
静态发布均保留，可通过静态链接回滚，不需要重启 Manager。
