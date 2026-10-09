# jp-meta 理论净值 IP 排除配置修复（2026-10-08 UTC）

理论净值页面重复显示
`read IP exclusion config /home/ubuntu/bitget-intra-arb01/trade_engine.toml`。
现场确认以下两个部署已停止，部署目录及交易配置文件均不存在；操作员也确认
这两个盘子已停：

- `/home/ubuntu/bitget-intra-arb01/trade_engine.toml`
- `/home/ubuntu/gate-intra-arb01/trade_engine.toml`

Manager 的 `kline.trade_engine_configs` 仍引用这些旧路径。每次补行情前都会
检查交易 IP 排除配置，文件缺失使检查失败并阻止新的 Binance K 线请求。
多币种补行情的错误又被 `warm_ranges` 收集后拼入 `unavailable_reason`，所以
浏览器出现同一句错误多次。没有放宽文件读取失败时停止行情请求的保护。

修改 jp-meta 的 `/home/ubuntu/crypto_cta_manager/config/cta-manager.toml`，仅
移除这两个已失效的列表项，排除配置由 24 项变为 22 项。其余配置经 TOML
解析比较完全一致，保留行情专用本地 IP `172.31.46.93`、预期公网出口
`18.181.48.65`、全部七个交易公网 IP 排除项、600 权重/分钟预算及 HTTP
并发上限 8。所有运行中的 `trade_engine` 配置仍被排除清单覆盖。

修改前备份及验证证据位于远端：

```text
/home/ubuntu/crypto_cta_manager/config/backups/kline_exclusions_20261008T061349Z/
```

备份目录权限为 `0700`，配置备份权限为 `0600`。只重启
`crypto-cta-manager-web.service`，继续使用原有二进制及前端发布
`20261006T041103Z`。22 份交易 TOML 的 SHA-256 及 107 个交易、持久化、
Viz、Config、监控、Nginx 和 Redis 进程的 PID/命令摘要均与修改前一致。
不将 PostgreSQL 临时连接进程纳入固定 PID 比较。

重启后通过现有 4191 Nginx 网关检查：Manager 健康状态为 `ok`；四个独立
账户的最新 1D 实际净值各返回 98 个点；默认行情维护成功补入 100 根分钟
K 线，`kline-status.last_error` 清空。理论目标索引按正常冷启动流程重建，
实际净值在索引初始化期间可用。

目标索引在 `06:28:04 UTC` 重建完成，耗时 `720603 ms`，本次初始化处理
`3667034` 条消息。日志显示 `initial=true`、`invalidated=false`：这是新进程
建立内存缓存，不是历史归档损坏。`processed_messages` 是累计处理次数，
增量更新会重复读取最近两分钟的尾部，不能将重启前的累计值当作归档总条数。

索引就绪后使用当时的当前时钟查询四个账户各自的最新 1D 区间：
`startMs=1791354500263`、`endMs=1791440900263`、`maxPoints=200`。

| 账户 | 实际净值点 | 理论净值点 | 缺失价格 | 理论加载中 |
| --- | ---: | ---: | ---: | --- |
| trade01 | 98 | 98 | 0 | false |
| trade02 | 98 | 99 | 0 | false |
| trade03 | 98 | 104 | 0 | false |
| trade04 | 98 | 99 | 0 | false |

四个理论结果均无不可用原因。默认维护及按需补齐累计发出 140 次行情请求，
缓存新增 21530 根分钟 K 线；紧接着重复同一组查询，新增行情请求为 0。
最终再次核对 107 个受保护进程、22 份交易配置及 Manager 二进制哈希，均
未变化。通过网关验证所创建的临时认证会话已删除。

理论索引就绪并完成上述查询后，Manager 的 `/proc/<pid>/status` 实测：
RSS `10347388 KiB`（约 9.87 GiB），本次进程峰值 RSS `13008860 KiB`
（约 12.41 GiB），其中匿名常驻内存 `10059424 KiB`。主机总内存约
188.48 GiB、可用约 142.39 GiB。这些是整个 Manager 进程的占用，包含
实际成交/FIFO、理论目标索引及行情缓存，不能归因为理论索引单项的大小。
理论目标索引仍保留全历史增量，常驻缓存随历史增长的开销及冷启动耗时是
后续需要优化的问题；本次没有修改缓存模型或发布新的二进制。
