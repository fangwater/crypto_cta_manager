# 运行以来净值监控：2026-10-28

按操作员最终口径，各盘从现有可重建监控起点归零，展示累计变化，不推算初始
权益。常规采样点为上海时间每四小时，并保留起点及最新点。FR 排除浮动盈亏；
CTA 保留既有 FIFO 浮动及当前资金费选择；MM 沿用 NAV 原有金额 FIFO 总收益。

## 发布

- CTA runtime：`eaea52a`；最终 frontend：`8cd23c4`。
- NAV runtime/interface：`464872b`（主体接口 `76633c9`）。
- jp-meta NAV：`/home/ubuntu/crypto_nav_manager`，仅重启
  `crypto-nav-manager.service`；相关带迁移的工具二进制随主程序一并替换。
- jp-meta/SG CTA：`/home/ubuntu/crypto_cta_manager`，仅重启各自的
  `crypto-cta-manager-web.service`。最终静态发布目录为
  `web-releases/nav-monitor-20261028T034509Z-final`。
- 保留现场 TOML、服务单元和 CPU affinity；jp-meta CTA 保持 CPU 24，
  SG 保持 CPU 0–7。全部构建在本地 NVMe target 完成。

NAV 提供机器认证的
`/nav-api/integrations/fr-account-overview/nav?sourceId=<slug>`；CTA 通过 HTTP
拉取，按总览相同权限返回 `/manager/api/account-overview/nav?sourceId=<id>`。
SG 通过自己独立的 `/manager/api/integrations/account-overview/nav` 导出允许的
本地账户，不递归导出外部来源。契约与估值说明见 `fr_account_overview.md`。

## 验证

- CTA `cargo check --all-targets` 通过；库测试 440 通过、6 忽略。四小时网格、
  来源身份、零起点、缺失点校验均通过。最后的采样上限修正另跑编译及接口契约测试。
- NAV 总览专项 7 项、PnL overview 专项 3 项通过，覆盖 FR 排除浮动、费用/
  资金费/利息只计一次、MM 原模型及无初始权益时的历史输出。
- 两项目 release 构建成功；CTA 前端 TypeScript/Vite production build 成功。
  Oxlint 仅有原有的 AuthGate/Field 未使用变量警告。
- 网关 `/manager/` 及其 JS/CSS 资产、NAV health 均返回 200。
- 用短期验收会话逐账户请求；结束后删除这些会话。lgq 总览可见 19 个账户，
  prc 可见 4 个。prc 直接访问受限外部净值返回 403，无法绕过主机可见上限。
- 验证响应的首点为零、时间有序、金额有限、首尾与范围一致、常规点为四小时
  网格；complete 曲线不得缺失中间点。NAV Binance FR 历史已覆盖 2354 个点。
- 最终逐账户验收取得 23 份有效曲线响应（lgq 19 份、prc 4 份）；12 个当前
  可见 NAV 账户全部有点且 complete，SG 的 HTTP 转发曲线正常。原已排除的
  Gate 两个来源继续遵循现场排除配置，不扩大可见范围。
- 受保护进程：jp-meta 128 个、SG 32 个 PID/启动时间一致；配置哈希分别
  37 份及 9 份一致。
- 四张 immutable 快照表的行数及整表内容哈希在发布前后一致：jp-meta
  `4/122/4/131` 行，SG `2/12/0/0` 行。

## 运行状态与限制

NAV 后台单任务重放，重启后按账户逐个就绪，浏览器对 loading 自动重试。
每个来源独立返回，不因其他账户缺失而失败。异常输入会返回暂不可用并后台重试，
不永久冒充加载中。资金费估算缺少早期覆盖时保留可用部分并明确标注；SG 目前
未接入资金费，显示交易费后累计变化及对应说明。

CTA 总览的曲线支持已有账户筛选和独立缩放。每日日期选择只作用于原每日
表，运行以来曲线有独立说明。历史超过分钟缓存保留期使用完整成交价估值并标记；
不裁短历史、不混合不同估值口径，不将未知资金费伪装成零。

发布的初次存活检查误用了需登录的 CTA `/api/health`，触发了一次自动回滚；
修正为公开的 `/api/auth/status`，并等待 jp-meta 冷历史加载后完成发布。
交易相关服务未被重启，现场配置、快照及最终 CPU affinity 保持一致。

## 账户展示修正

按操作员反馈，总览改为每账户独立的净值卡片；每张图只包含该 source_id 的
曲线，并独立展示累计变化、时间范围、估值/资金费口径和数据状态。桌面双列、
移动端单列，缩放各自独立。去除跨账户叠加图以及总权益、今日总盈利，账户
搜索与选择移到图表上方。保留已有每账户每日指标表及源接口、权限和计算口径。
此修正只发布 jp-meta 前端静态文件。

修正版本 `66c7fdd` 已发布到
`web-releases/account-nav-20261028T044007Z`；runtime 保持 `eaea52a`。
实际站点桌面和手机验收通过：lgq 的 19 个账户及 prc 的 4 个账户均有
各自独立的图表实例，搜索筛选正常，页面无横向溢出。前端 production build、
4 项 workspace 测试及 lint 通过（仅既有两处未使用变量警告）。发布前后
130 个受保护进程身份和启动时间、37 份配置哈希、后端二进制及 CPU 24
均一致；未重启服务。临时验收登录会话和浏览器数据在验收后删除。


## 独立栏目与整点持久化修正

总览恢复 `f0ef2ea` 的原页面，保留原有账户表、统计和筛选；累计净值移到
独立「净值」栏目 `/manager/net-value/`。原详细 CTA 页面继续保留。
后台对齐上海时间 00/04/08/12/16/20 点，按固定截止时间计算并原子持久化
至 Manager 自己的 RocksDB `nav_monitor` 列族；保留真实零起点，移除移动尾点。
外部来源继续通过 HTTP 获取，后台校验连续整点后保存。本地和外部的页面查询
均仅读取已保存缓存，不触发逐笔重放或 HTTP 上游请求。新一期未就绪时保留
上一期并标记后台更新，失败按账户重试，重启直接恢复已保存曲线。

本次不修改 PostgreSQL schema、初始持仓快照、NAV/Exec 运行时或交易配置。
本地 `cargo check --all-targets`、443 项库测试（6 项忽略）、4 项 workspace
测试及前端 build/lint 通过，lint 仅两处既有未使用变量警告。
