# jp-meta 成交成本复盘发布（2026-10-13 UTC）

发布标识：`20261013T034926Z`。仅替换 `cta_web` 和前端资源。
前端目录为 `/home/ubuntu/crypto_cta_manager/web-releases/20261013T034926Z`。
既有服务单元、在线 TOML、监控、辅助工具和 Nginx 配置没有替换。

构建来自现有工作区，基于 `bf6b3542bd7308cd9d6b2b20bc646a25c476cd35`
及未提交改动。发布目录中的 `RELEASE-MANIFEST.json` 记录源码与产物哈希，
不将这个构建冒充为纯 HEAD 构建。线上原二进制已经包含本地的
Binance/RapidX 账户总权益读取修复。

## 切换

上传后逐文件校验 SHA-256，再原子替换后端二进制。后端就绪后原子切换
`webroot`。首次就绪等待过短，历史扫描未完成时触发自动回滚；随后延长
等待重新发布。新版最终启动时间为 `2026-10-13 04:12:29 UTC`，
Manager PID 为 `1872068`。没有修改冷启动逻辑或数据。

运行二进制 SHA-256：

```text
bf60725a67c04a68cc6758f953c92187c69a1a8034fcde7be9ef09c04d923f26
```

## 验证

- 本地 Rust 全量测试、release 构建及前端构建通过，7 项前端分析测试通过。
- 公网成本页面、JS、CSS、favicon 均为 HTTP 200，并逐文件匹配产物哈希。
  JS 包含新的复盘图及 `qty_weighted_target_delay_us` 字段。
- 升级后 Manager health/dashboard、trade03 Viz/snapshot/Config 均返回 200，
  Viz WebSocket 握手返回 101。
- 118 个受保护进程的 PID 和启动标识保持不变。
  实际在线 Kline 配置列出的 23 个交易 TOML，以及 Manager TOML、服务单元、
  Nginx 片段/站点配置和路由映射的哈希保持不变；Kline 预算为 600/min。
- 固定 trade03 区间：`startMs=1791776580000`，`endMs=1791862980000`，
  25 行/页。升级前包含 29,239 笔可比成交、39,208 个虚拟 delta，
  缺失 delta 为 0。

截至 `2026-10-13 05:20:43 UTC`，升级后的固定区间数值比对仍未完成。
目标历史索引状态为 `ready=false`、`loading=true`、`last_error=null`，
累计处理计数为 3,936,256。该计数包含重读，不能作为完成百分比。
临时只读检查得到归档估计键数约 315 万；精确键扫描未完成即已停止。
本次冷启动明显长于早期部署记录，没有把加载中响应算作成本验证通过。

最终确认运行中二进制哈希匹配发布包、health HTTP 200、118 个受保护
进程及配置仍不变。已清理临时会话、验证/诊断进程和发布暂存目录；
Manager 自身的后台索引加载继续运行。回滚二进制、原 webroot 路径和
固定区间基准报告保留在：

```text
/home/ubuntu/crypto_cta_manager/deploy-backups/acquisition-cost-20261013T034926Z
```

发布目录保留 `DEPLOYMENT-VALIDATION.json`，明确记录
`cost_comparison=pending_target_history_initialization`。
没有进行浏览器渲染验证。
