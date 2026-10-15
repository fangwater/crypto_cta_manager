# el01 bahll202210 30D 查询诊断 — 2026-10-15

检查对象为 el01 的 `binance_exec_trade01`（bahll202210）。未发现当前查询由主机内存耗尽、Manager OOM、重启或后台目标索引未就绪造成；复现到的确定问题是 30D 理论曲线的时间保留边界校验。

## 资源与服务

03:07–03:09 UTC 采样时，主机内存共 15,991 MiB，MemAvailable 约 4,166–4,185 MiB。`cta_web` RSS 约 9.4 GiB，进程交换约 36 MiB；主机交换空间使用约 1.8 GiB，短时间 vmstat 采样无持续 swap-out。内存 PSI 的 10/60/300 秒均值均为 0；Manager cgroup 的 `oom`、`oom_kill` 均为 0，`NRestarts=0`，PID 持续为 2605826。根分区尚余 45 GiB。

Manager 健康 API 为 `ok`，13 个 source 的周期刷新正常，最近几次用时约 11–13 秒。目标历史索引 `ready=true`、`loading=false`、`last_error=null`。诊断前后 Kline 请求数同为 399，没有活动回填。

这些数据说明当时没有主机内存耗尽的证据，但不能替代用户电脑的内存或现场浏览器状态检查。Manager 的约 9.4 GiB 常驻内存本身仍是后续可优化项。

## 原始 30D 窗口复测

通过现有 Nginx 的 `/manager/api/timeline` 重放访问日志中的同一窗口：`startMs=1789441355221`、`endMs=1792033355221`、`sourceIds=binance_exec_trade01`、`maxPoints=3500`。

| 查询 | HTTP | 总耗时 | 实际点数 | 理论点数 | 理论加载 |
| --- | --- | --- | --- | --- | --- |
| includeTheoretical=false | 200 | 0.423 秒 | 2882 | 0 | true，提示理论计算中 |
| includeTheoretical=true | 200 | 0.127 秒 | 2882 | 0 | false，保留范围拒绝 |
| 当前时刻的新 30D，includeTheoretical=true | 200 | 0.435 秒 | 2882 | 0 | false，保留范围拒绝 |

返回理由均为：`理论数据仅支持最近 30 天，不能查询更早或未来的区间`。响应约 5.64 MB；实际净值已完成。第一阶段的 `loading=true` 是前端先取实际净值、随后请求理论叠加的正常占位值，第二阶段已经明确结束。

`frontend/src/App.tsx` 的 30D 快捷按钮按固定结束时间减去整 30 天。`src/kline.rs::validate_range` 则按服务端处理时刻要求 `start >= now - 30 days`。即使客户端时钟准确，网络和计算时间也会使恰好 30 天的起点越界；结束时间在页面内固定后，这个偏差会继续扩大。`src/theoretical_nav.rs::load_timeline` 还要求起点之前的完整分钟基准价处于保留范围内。不能通过放宽验证来下载已过期 Kline，也不能把被拒绝的理论值当作零。

## 浏览器核对及限制

在 el_dev 的 Chromium 中创建独立浏览器上下文，加载 el01 已部署的实际前端 bundle，拦截 API 并重放本次真实诊断响应。点击 30D 后实际曲线正常绘制，`loading=false`，显示上述范围提示，没有 JS 异常、横向溢出或持续卡死；观察到的最长主线程长任务为 145 ms，JS 堆约 22 MB。

这是对部署 bundle 和真实响应的回放检查，不能证明操作员浏览器当时没有卡顿，也未验证修正起点后的全符号 30 天 Kline 完整覆盖。若现场表现是整个页面无响应或一直遮罩计算中，需继续核对现场请求/浏览器；不能仅凭本次检查把那种表现全部归因于保留边界。

## 操作边界与证据

最初诊断阶段未改动代码、运行配置、CPU 绑定、交易进程或目标，未重启任何服务，未变更持久业务数据。只插入了一个有效期 10 分钟的临时诊断登录会话，`finally` 中已删除；凭据始终留在远端，未输出。Chromium 独立上下文已销毁，el_dev 的两个临时回放文件及其专用目录已按精确路径删除并确认不存在。

本地证据：

- `artifacts/el01_bahll_30d_20261015/summary.json`：查询耗时、状态、边界理由。
- `artifacts/el01_bahll_30d_20261015/resources.txt`：资源与 OOM 计数。
- `artifacts/el01_bahll_30d_20261015/browser_result.json`：已部署前端的真实响应回放结果。
- 同目录 `probe_remote.py` 和 `browser_replay.mjs`：诊断脚本；大型 API 原始响应 `probe.jsonl` 和 `aligned_probe.jsonl` 不含登录凭据，已移至[仓库外本地归档](../artifacts/LOCAL_ARCHIVE_20261015.md)。

## 30D 修复与上线

操作员随后要求修复理论查询的 30 天边界。最终前端 release 为 `20261015T033651Z`，JS 资源为 `index-DHT1QDcZ.js`。只更新 el01 静态前端；后端二进制、live TOML、CPU 绑定、82 个受保护进程及 13 份交易配置保持原值，Manager PID 仍为 2605826，无服务重启。原前端 release `20261014T223814Z` 保留。

`frontend/src/lib/navRange.ts` 和 NAV 页使用理论 API 的 `available_from_us` 对齐 30D 快捷范围，完整五分钟执行窗口也必须处于保留区间。已有服务器边界时，点击按钮直接采用该边界；边界过时或后台回填期间向前移动时，根据响应重新对齐并重发实际/理论查询。时间输入同步显示真实起点，并显示“实际与理论净值使用同一起点”的提示。服务器成功返回完整理论曲线后保留该固定区间，停止边界调整与自动重试。

这会舍去保留边缘无法安全定价的几分钟，避免浏览器时钟、网络延迟、分钟前基准价或跨起点五分钟执行窗口导致拒绝。实际与理论均按调整后的区间重新计算；不单独裁剪或平移理论曲线。自定义、ALL、1D、7D 不会被自动截短，完全过期的区间仍被拒绝，服务器保留校验和禁止回填过期 K 线的规则保持不变。

`node --test tests/navRange.test.mjs` 的 6 项回归测试、`npm run build` 和前端 lint 通过；lint 有两个既有 unused-variable 警告。桌面/手机模拟检查覆盖提前对齐、过时服务器边界、后台补齐自动重试、完成后停止调整、保留事实图表、ALL 范围与账户隔离。

最终通过真实 Nginx、真实认证会话和已发布 bundle 的桌面 1440×1000、手机 390×1100 核验：

| 视口 | 实际点数 | 理论点数 | 理论缺价 | 服务端计算 | HTTP 接收及 JSON 读取 |
| --- | --- | --- | --- | --- | --- |
| 1440×1000 | 2882 | 1754 | 0 | 622 ms | 3890 ms |
| 390×1100 | 2882 | 1754 | 0 | 355 ms | 3836 ms |

两次均 `loading=false`、`unavailable_reason=null`，实际与理论首点为同一时间且净值为零，两条理论颜色均实际绘制，没有 JS 异常或页面横向溢出。每个完全相同窗口的重复查询，实际报表和理论点精确一致，Kline 请求增量为 0，扣费前后差额等于估算费用。HTTP 耗时包含约 6 MB JSON 经现有入口传输和读取，不能当作服务端计算耗时。

临时认证会话在 `finally` 中删除，浏览器使用独立上下文并销毁，未写认证文件。临时回放资源与截图在检查后按精确路径删除。最终证据见 `artifacts/el01_bahll_30d_20261015/deployment.json`、`release_manifest.json`、`live_summary.json`、`browser_live_result.json`、`browser_fix_result.json` 及部署前后运行快照。
