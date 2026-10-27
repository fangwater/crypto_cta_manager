# jp-meta 账户信号配置入口与旧 trade05 退役

2026-10-27 按操作员要求，从 jp-meta 当前 Manager 主机 TOML 中移除已禁用的
`binance_exec_trade05` / `lgq_cta_RX02` 来源。账户列表由 9 项变为 8 项。
保留 PostgreSQL 中该来源的目录、历史分配校准，以及 Exec RocksDB 和历史身份；
没有删除数据库记录或运行目录。当前 RX04 的来源仍是
`binance_exec_rx04_5m05`，其 `/exec_trade05` 网关保留，不与旧来源混用。

原配置和验证记录位于主机：
`/home/ubuntu/crypto_cta_manager/backups/hide_trade05_20261027T043430Z/`。
仅重启 Manager，CPU 24 保留；163 个受保护进程和 29 份交易/Nginx 配置保持不变。
登录后的账户 API 确认旧来源消失、其余 8 项完整保留；Manager health 为 `ok`，
RX04 snapshot 返回 200。临时验证会话已删除。

账户页 `/manager/accounts/` 在以下账户的“执行监控”后增加“信号配置”链接：

| source_id | 信号配置路径 |
| --- | --- |
| `binance_exec_trade06` | `/cta/binance-cta-spread-rx01/config/` |
| `binance_exec_trade08` | `/cta/binance-cta-perp-rx02/config/` |
| `binance_exec_trade07` | `/cta/binance-cta-perp-rx03/config/` |
| `binance_exec_rx04_5m05` | `/cta/binance-cta-perp-rx04/config/` |

这是 jp-meta 专用前端补丁，以明确的 source_id 映射，仅在入口
`http://13.115.227.29:4191` 展示，避免影响其他主机的同名来源。
trade08 使用 RX02 信号配置；不从显示名称或可复用的网关编号推断信号身份。

`npm run build` 和 `npm run lint` 通过；lint 保留两项既有未使用变量警告。
前端发布为 `signal-config-links-20261027T043802Z`，发布目录保持
`manager/index.html`、`manager/assets/` 布局，原子切换 `webroot`。
账户页、主页、workspace 和四个静态资源均通过公网 200 与 SHA-256 校验，
四个信号配置页均为 200 且标题正确。未执行浏览器渲染检查。
此次前端发布没有重启服务，含 Manager 的 164 个受保护服务进程身份与 CPU
绑定均保持不变，后端二进制和主机 TOML 的哈希保持不变。

发布清单、源码补丁和两阶段验证结果见
`artifacts/jp_meta_signal_config_links_20261027/`；远端前端回退记录位于
`/home/ubuntu/crypto_cta_manager/backups/signal-config-links-20261027T043802Z/`。
