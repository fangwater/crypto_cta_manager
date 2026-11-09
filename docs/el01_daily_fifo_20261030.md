# el01 每日 FIFO 与七天内存窗口发布

2026-10-30，操作员授权部署。目标为 SSH `cta_exec` 的
`el-jp-yf-srv-7-6`，Manager 根目录 `/home/el01/crypto_cta_manager`。
用户入口 `http://172.16.30.42:10041/manager/` 是已有 SSH 转发，连接该主机
Nginx。没有部署其他主机，也没有更新前端。

最终发布目录：`releases/daily-fifo-final-20261030T101324Z/`；程序 SHA-256：
`fecb8a4384171967cb9d69ab5af8b34fb71523ec7a9011b74f144386a13d8a73`。构建基于 `cc7542c` 及本次工作区修改，
发布目录的 `source_manifest.json` 记录所有涉及源文件的哈希。

## 实现

Manager 自有 RocksDB 的 `nav_fifo_daily` 保存上海日界 FIFO，键为
`source_id || NUL || boundary_us(be64)`。账户与策略状态原子保存，包含未平仓
批次、累计实现、费用、估值及读取水位。内存只保留最近七个完整日及当天的
订单尾部、必要 FIFO 状态和少量近期查询结果。长窗口按需恢复，不缓存完整
历史结果；并发历史回放共享预算锁。Linux/glibc 在回放后尽量归还空闲页。

90D/ALL 保持完整请求语义，起点只受该账户真实历史起点限制，不能缩短到七天
或分钟价格保留范围。超过分钟保留范围的整条曲线仍标识为成交价估值。
没有初始快照的账户保留完整首次成交时间。设计与冷启动限制见
`nav_daily_fifo_design.md`。

## 配套维护与回滚

仅替换 Manager 的 `bin/cta_web` 并重启 `crypto-cta-manager-web.service`。
验证时发现两条 Manager Nginx 路由仍为 15 秒超时，导致后台尚在计算时返回
HTTP 504。只把 `/manager/api/` 和 `/cta-api/` 的 `proxy_read_timeout`
改为 180 秒，经 `nginx -t` 后平滑重载；Nginx 主进程保持，工作进程正常更替。

当前代码的账户别名功能已存在于仓库，但 el01 未应用对应维护脚本，账户页
因此报缺少 `display_alias`。先备份 `cta_order_sources`，再显式执行
`scripts/apply_account_alias_schema.sql` 添加可空展示字段。15 行的全部既有
字段逐项比较一致。普通启动没有 DDL；没有修改授权、账户绑定或持仓快照。

最初部署前的回滚文件位于：

`/home/el01/crypto_cta_manager/backups/daily-fifo-20261030T094928Z/`

- `cta_web.previous`：原运行程序的硬链接。
- `cta-manager.toml.previous`：原 Manager 配置。
- `nginx.conf.previous`：原网关配置。
- `cta_order_sources.before_alias.json`：补列前账户目录，权限 0600。
- `apply_account_alias_schema.sql`：本次实际执行的维护 SQL。

旧程序 SHA-256：
`741bd80a5ccbc7dfcc27e3ba9e8223e674ce60e06740c7a11ca8dd1c68bdaca0`。
回滚程序时可以保留新增的可空字段和可重建的 Manager FIFO CF；不要删除
历史数据、快照或 Exec 文件。每次程序替换都另存了前一版程序及清单。

## 验证

本地 `cargo fmt --check`、`cargo check --offline`、
`cargo test --offline -- --test-threads=4` 和
`cargo build --offline --release --bin cta_web` 通过，480 项测试通过，24 项按
原有条件跳过。覆盖完整重算与日界恢复、日界成交、receive/fill 跨日、晚到
成交、费用失效、未平仓旧批次、账户/策略、资金费与导出一致性，以及无快照
历史起点和 7D 请求传输延迟边界。

现场验证记录保存在本地 `artifacts/el01_daily_fifo_20261030/`，其中包括
发布前固定一小时窗口的 14 个账户 × 两种分析模式、过程及最终验证、内存
采样和发布清单。所有临时诊断登录会话都在脚本 finally 中删除。

带内存回收的版本完成一次 90D → 7D → 90D 检查，使用
`includeTheoretical=true`。首次 90D 策略/账户分别约 38/27 秒；7D 账户首次
缺少分钟价格，后台补齐后重试成功。90D 使用成交价估值，7D 使用完整分钟价。
返回曲线通过首末端点、汇总、策略及币种加总检查。

发布前 Manager RSS 约 12 GiB。带回收版本的历史查询结束后观测到约
4.7–6.6 GiB，期间包括七天分钟价缓存及目标索引初始化；这不是固定内存上限。
trade03 原始常驻事件从约 184 万裁到 28.6 万，trade07 从约 415 万裁到
85.6 万。旧 FIFO 未平仓批次仍必须保留。

最终版本的 28 份固定窗口完整报告哈希与发布前全部一致。首轮发现的七份
差异来自无初始快照账户的 `earliest_start_ts_us`，已修正并验证，未改变盈亏。

88 个非 Nginx 受保护进程的 PID、启动时间和 CPU 绑定全部保持，Nginx 原有
主/工作进程仍在，新工作进程由平滑重载产生。16 份其他配置、6 张持仓/归属/
日权益表的行数与内容哈希一致，账户资金费设置、前端指向、Manager CPU
绑定（0–7）均保持。正常运行后 `NRestarts=0`，刷新无错误。

最终程序再次请求相同固定窗口，四份完整报告也与前一轮一致，均一次成功：

| 窗口 | 策略视图 | 账户视图 | 估值 |
| --- | ---: | ---: | --- |
| 90D | 30.25 秒 | 27.21 秒 | 完整历史成交价估值 |
| 7D | 22.08 秒 | 24.86 秒 | 完整一分钟收盘价 |

90D 返回后 RSS 分别约 4.54 / 4.79 GiB；7D 返回后约 5.54 / 6.87 GiB。
随后重复 7D 事实曲线（关闭理论覆盖）分别约 10.91 / 8.64 秒，报告保持一致。
10:21 UTC 观测 RSS 约 5.16 GiB，最终进程观测峰值约 7.21 GiB，进程 swap
为零。目标历史索引完成 4,960,930 条消息，Kline 后台任务为零、无错误。
这些是现场短期观测，不是长期或任意账户规模的内存保证。

本次冷启动约四分钟；仍保留逐账户核对完整事实来源的行为，尚未实现跨重启
验证后直接采用磁盘边界的快速启动。正常增量刷新和日界查询已生效。
