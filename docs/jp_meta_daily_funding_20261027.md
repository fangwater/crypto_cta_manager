# jp-meta 每日 Funding 费用快照

2026-10-27 按操作员要求，直接计算每日估算 Funding 并保存费用分项，
不建立原始公开费率的持久化历史。部署范围仅 jp-meta。

`[funding_rates].daily_estimates_enabled = true` 启用后台累计及账户卡片展示。
其他主机默认关闭。每日按上海 00:00 拆分，重用 NAV 的持仓锚点、已验证成交、
策略归属和结算整点前数量；既有公式、费率出口、限流预算、实际采集开关不变。
已结束日保存到 Manager 自有 `funding_daily` CF，今天实时估算。
完整币种日冻结，部分日只保存已知币种，后到成交或归属修正按输入指纹重算。
累计读取历史费用日加今天；不重复加实际账单和估算。

账户页保留订单净值，单独展示 Funding 的实际/估算类型和金额，主数值含选定的
Funding。RX01/trade06、RX03/trade07 保持优先实际账单；其他账户用估算。
历史费率正在后台按原预算补齐，缺失保持未知，页面展示补齐/部分覆盖状态。
这次验证不宣称全部历史及币种已经完整覆盖。

前端发布目录为
`/home/ubuntu/crypto_cta_manager/web-releases/daily-funding-20261027T052904Z`。
最终后端 SHA-256 为
`eac49e1bcda5f1c8ac16751973a220d3a270b1822e8ec1c563a2de7cc5e2a58d`。
仅替换 Manager 后端及前端，并在现有主机 TOML 增加上述开关；未用仓库模板覆盖
主机配置。旧 trade05 仍已移除，RX04 网关保持原状。最终 Manager PID 2603414，
CPU 24。原二进制、配置与前端链接的回退资料位于
`/home/ubuntu/crypto_cta_manager/backups/daily_funding_20261027/`。

验证：

- `cargo fmt --check`、`cargo check`、全量 `cargo test`、release 构建通过。
  库测试 431 通过、5 项按既有条件跳过；另有 18 项二进制测试通过、16 项需要
  独立 PostgreSQL 的集成测试跳过。本次没有 SQL schema 变化。
- 新测试覆盖费用落盘/重开/幂等/source 隔离、实际与估算不相加、未知不置零，
  上海日界线逐日求和等于整段计算，以及后到成交仅使其后日期指纹失效。
- `npm run build`、`npm run lint` 通过，保留两项既有 lint 警告。
  公网两项入口静态资源与发布文件 SHA-256 一致；没有运行前端开发服务器。
- 8 个账户相同固定区间的订单 NAV 报告哈希均与发布前一致。
  163 个受保护进程、29 份交易/Nginx 配置、Funding 开关及轮询设置、CPU 绑定不变。
- 四张持仓快照表、21,095 行归属校准及 182 行每日权益记录的行数和整表哈希均不变。
  没有写入 Exec RocksDB、执行 PostgreSQL DDL 或重写原权益锚点。
- 通过一次性只读 RocksDB secondary 读取到 200 个费用日，覆盖 8 个 source。
  此时均为部分覆盖；完成检查后移除 secondary 元数据。
  5 个账户的已知 XRPUSDT 费用日金额与同日、同币种直接查询逐一一致，
  保存行的币种金额及策略金额分别加总到该行账户金额。

核对数据见 `artifacts/jp_meta_daily_funding_20261027/`。
