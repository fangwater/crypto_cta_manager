# el01 多 Virtual 叠加发布 — 2026-10-22

操作员要求提交、推送本次修改，并发布到 el01。功能提交为 `d1cd2c8`，已推送
`origin/main`。使用与 jp-meta 验收一致的 Manager 二进制和前端；额外重新编译
`nav_rebuild`、`nav_snapshot`、`nav_strategy_snapshot`，供标准 Manager-only 发布脚本部署。

运行前完成 `cargo fmt`，确认源码、二进制、前端文件散列与已通过全量 Rust 测试、
16 项 PostgreSQL 集成测试和前端构建的版本一致。敏感值检查未发现嵌入凭据。

## 数据库与配置

发布前 el01 有 14 个启用账户、9 个 Virtual、6 条跟随关系。保留原有关系与倍率：

| 账户 | Virtual | 倍率 |
| --- | --- | ---: |
| trade04 | virtual06 | 150 |
| trade10 | virtual03 | 171 |
| trade11 | virtual03 | 261 |
| trade13 | virtual03 | 17.8 |
| trade14 | virtual07 | 100 |
| binance_exec_xy_lxy21_05 | virtual01 | 118 |

完整 PostgreSQL 备份 506,663,704 字节，只保留在主机临时备份目录。预检发现
el01 尚无当前运行代码必需的 `cta_strategy_allocation_snapshots` 表；按既有
`scripts/apply_allocation_schema.sql` 创建空表，并与
`scripts/apply_multiple_virtual_follows_schema.sql` 合并到一个显式事务执行。
跟随表主键变为 `(source_id, virtual_id)`，原有行与有效绑定散列保持不变。
没有插入所有权修正、改写快照或发布测试目标。

原 Nginx 对 Manager 没有强依赖，服务文件与仓库一致。使用
`scripts/deploy_host.sh --target el01 --skip-build --manager-only`，仅重启 Manager，
不覆盖运行 TOML。前端、分析工具和模板文件按脚本更新。

## 验收范围

发布前全账户分钟估值请求已存在 `nav_pricing_unavailable` / `loading=true`；
当时 Kline 有 192 个后台回填，无最近错误。trade01、trade04 以及 trade09 的
BTCUSDT 小窗口均未返回完整价格覆盖。终止本次诊断重试后，临时登录会话已删除。
因此没有取得可供发布前后数值比较的固定窗口 NAV，不宣称该比较通过。

发布后核对全部启用账户的配置接口、跟随行、有效绑定、前端网关资源散列、
交易/Viz/Config/Nginx/监控进程、配置文件和四张原有快照表，均通过。

发布目录为 `/home/el01/crypto_cta_manager/web-releases/20261022T095502Z`。
Manager 于 09:56:14 UTC 启动，PID `3230777`；健康检查正常，自动重启次数为零。
全部 14 个账户的配置接口与 PostgreSQL 一致，6 条跟随关系返回单元素 `follows`，
9 个 Virtual 正常列出，发布队列全部为空。前端 4 个资源逐项通过 Nginx 散列核对。

90 个受保护进程、16 份交易/Nginx/Manager 服务配置、四张原有快照表、五张
Virtual/有效绑定表、运行 TOML、Funding 开关和 CPU affinity 均与基线一致。
新建的所有权修正表保持零行。没有启停交易、Viz、Config、Nginx 或监控服务。

发布后重查同一固定窗口，trade04、trade09 均成功返回完整分钟估值 NAV，
各 98 个采样点；Kline 回填任务归零且无最近错误。当前 Manager 公共 Funding
错误数为零。发布前缺少完整基线，仍不把此结果描述为数值前后完全一致。
理论目标历史索引继续按正常重启流程在后台重建。

Manager 二进制 SHA-256：

```text
ccb2fb93b137e9363d4e235ce764dd37d39c97b67b861987a1355beacdce1269
```

证据目录：`artifacts/el01_multiple_virtuals_20261022/`。数据库备份、凭据和会话值
不进入仓库；主机临时备份及暂存文件已删除并确认不存在，保留发布清单和验收记录。
