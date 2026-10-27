# el01 每日 Funding 费用快照发布

2026-10-27 按操作员要求，将 jp-meta 的每日 Funding 功能发布到 el01。
运行代码为 `dbb86356fa45b72bf3c6df286735d81b190c7aea`，后端 SHA-256
`eac49e1bcda5f1c8ac16751973a220d3a270b1822e8ec1c563a2de7cc5e2a58d`，
与 jp-meta 已发布后端一致。前端目录为
`/home/el01/crypto_cta_manager/web-releases/daily-funding-20261027T060024Z`。

仅替换 Manager 后端、前端，并向现有主机 TOML 增加：

```toml
[funding_rates]
daily_estimates_enabled = true
local_ip = "154.197.32.10"
```

公开费率出口显式绑定预留数据地址 `.10`，公网出口实测一致。没有用仓库模板覆盖
主机 TOML。现有 Kline 出口、1200 weight/minute 预算、并发 8、实际 Funding
采集开关和轮询设置不变。trade09 继续使用实际收入，其余 13 个账户使用估算。
每日估算写入 Manager 自有 RocksDB 的 `funding_daily`，保留上海日界线、持仓锚点、
成交归属与实际/估算不重复相加的规则。原前端 assets 随新目录保留，供旧页面加载。

发布前漏检了已有运行版本与新版本之间的归属校准 schema 依赖。前两次启动因
`cta_strategy_allocation_snapshots.restatements` 缺失而失败；失败后的后台索引
退出等待延长了 API 不可用时间。06:06 UTC 备份该表后，显式执行仓库现有
`scripts/apply_allocation_restatements_schema.sql`，仅补充默认 `[]` 的非空 JSONB
列。该表前后均为 0 行，未修改历史记录。第二次启动在补列前已经失败，等待后台
索引结束后于 06:08:48 UTC 自动重启；后续启动及健康检查正常。最终 Manager
PID 为 `3662337`，CPU affinity 仍为 `0-7`。

可回退的旧二进制、主机配置、前端链接及补列前表备份保存在
`/home/el01/crypto_cta_manager/backups/daily-funding-20261027T060024Z/`。
回退旧程序无需删除新增列。部署暂存目录和只读 secondary 的精确路径已清理。

验证结果：

- `cargo fmt --check`、`cargo check --locked`、`cargo test --locked`、release
  后端构建和 `npm run build` 通过。库测试 431 通过、5 跳过；二进制测试 18
  通过；16 项独立 PostgreSQL 集成测试按既有条件跳过。
- 14 个账户同一固定区间的 `timeline`、`account-timeline` 共 28 份订单 NAV
  报告完整哈希与发布前一致。
- 90 个受保护进程的 PID、启动标识、CPU affinity，以及 16 份交易/Nginx/Manager
  unit 配置哈希不变；仅 Manager 重启。
- 四张初始持仓快照表、归属校准表和 411 行每日权益审计记录的行数与整表哈希
  均不变。没有写入 Exec RocksDB，也没有重写权益锚点。
- 14 个账户均返回有值的累计 Funding。只读 secondary 检查时已保存 522 个账户
  费用日，覆盖全部 14 个 source；每行币种金额及策略金额分别加总到账户金额。
  5 个账户同日 XRPUSDT 保存金额与直接 NAV 查询估算逐一一致。
- 以上累计历史仍为部分覆盖；trade09 的实际账单覆盖也保留既有不完整标识。
  缺失部分保持未知，历史费率继续按原预算后台补齐，不宣称全历史已完整。
- Nginx 实际提供的 4 份前端文件与本地发布文件 SHA-256 一致。未启动本地开发
  服务器。诊断鉴权 session 和 RocksDB secondary 已删除。

发布清单、前后基线和数值校验见 `artifacts/el01_daily_funding_20261027/`。
