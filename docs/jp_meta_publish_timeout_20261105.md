# jp-meta 目标发布并发与启动账户模式校验

2026-11-05，操作员要求将 el01 已验证的目标发布修复部署到 jp-meta。
修复提交为 `2dee84a`：不同策略并发，同策略保持完整发布顺序；已入场任务
在 HTTP 客户端断开后继续完成；账户模式在启动时按来源校验，正常发布只读
内存结果，失败或缺少结果仍阻止非零目标，全零清仓继续允许。

## 构建与部署边界

通过 `jp-meta-elvpn` 核对主机为
`ip-172-31-35-228.ap-northeast-1.compute.internal`，用户 `ubuntu`，
Manager 根目录 `/home/ubuntu/crypto_cta_manager`。只替换 `bin/cta_web` 并
重启用户服务 `crypto-cta-manager-web.service`，前端、Nginx、Monitor、
交易/Viz/Config、源身份、绑定、目标、快照及交易出口配置均不修改。

使用当前工作区和既有 NVMe Cargo target，构建命令：

```sh
cargo build --locked --release --no-default-features --bin cta_web
```

该参数沿用 jp-meta 当前发布，不启用开发中的 daily-FIFO 与
`serde_json/float_roundtrip`，避免改变既有归属修订指纹的数值解析。
没有放宽、删除或改写指纹校验。切换前，新二进制通过 `--check-allocations`
只读预检。`cargo fmt --check`、相同 feature 集合的 `cargo check` 和
完整 `cargo test` 通过，509 项通过，26 项按既有环境条件跳过。

与 jp-meta 11 月 4 日当前运行版本源码清单比较，除这次发布修复外，
`config.rs`、`kline.rs`、`funding.rs` 还包含已在 el01 发布的显式共享行情
出口开关。jp-meta 现有 TOML 未启用 `allow_trading_ip`，默认仍为
false，保持专用本地 `172.31.46.93`／公网 `18.181.48.65`、600 weight/minute
和交易出口排除。签名资金费继续保留独立出口校验。其余后端源码与既有发布
清单一致；没有覆盖 host TOML 或前端。

发布为 `publish-startup-20261105T051904Z`。
旧二进制 SHA-256：
`927059b2cd7ea73985bbc8b87d58f72f16ae312285f5962370f83b6bc2c9b1d7`。
新二进制 SHA-256：
`dce67752969bb112a81602d905d07b52a10167fb727f409baf5cd6798f4cdcd9`。
回滚备份保存在
`/home/ubuntu/crypto_cta_manager/backups/publish-startup-20261105T051904Z/`，
包含旧二进制、Manager TOML、unit、源码清单与部署前基线。
新 Manager PID `1239090`，保持 CPU 24。

## 校验记录

05:20:07.034–05:20:07.222 UTC，8 个启用的可发布来源启动检查全部通过。
两个 RapidX 来源沿用既有 portfolio 配置检查，不调用 native 账户模式接口；
其余来源的请求显式绑定各自交易 IP，正常发布不再发起这些查询。

05:21:55.491 UTC，初始报表加载完成、API 开始监听；此前启动窗口内网关
曾返回 502。没有手工重放旧信号。至 05:26:52 UTC，恢复后约 297 秒内：

- Manager 日志记录 571 次完成请求，全部 HTTP 200；网关独立采样记录
  568 次 POST，全部 200，无 499/502。该观察窗口内无 WARN/ERROR。
- 完成请求耗时中位数 19 ms、P95 96 ms、最大 1,569 ms，没有超过 5 秒的
  请求；记录的排队耗时均为 0 ms。
- `rbf_big` 自然发布到 trade01–04。通过现有网关回读四个账户的 Exec
  Config，各有 26 个非零目标，更新时间均晚于本次重启。私有 RX03 的
  `cta-last-20-prep02-bn-5min-pos` 回读为 4 个非零目标；RX01 也有自然发布
  成功日志。没有注入测试目标或触发人工订单。
- 17 份固定一小时窗口报告（8 个来源的账户/策略各一份，加组合账户）
  部署前后报告 SHA-256 全部一致。窗口为 UTC 03:00–04:00、全币种、
  `maxPoints=60`、`includeTheoretical=false`。
- 154 个受保护进程身份和 CPU 亲和性、34 份配置摘要、11 张表摘要、资金费
  开关、BNB/BFUSD 设置和前端指向均保持一致。来源目录表只排除正常启动
  会刷新的 `updated_at` 后比较。prc 对四个私有来源的直接查询仍返回 403。
- prc/lgq 健康均为 `ok`，无报表刷新错误。历史目标索引处理 8,014,934 次
  消息后已就绪：`ready=true`、`loading=false`、`last_error=null`。
  Manager 为 `active`、`NRestarts=0`，PID `1239090`、CPU 24。

部署前另观察到 05:15:07 UTC，旧版本 `rbf_big`/trade01 出现一次
`Redis request timed out after 2s`，这是单独记录的 Redis 发布失败，不能
归因为账户模式查询，也不能仅凭新账户模式逻辑宣称 Redis 故障消失。

操作证据位于 `artifacts/jp_meta_publish_startup_20261105/`，包含主机基线、
生产构建日志、部署回执、固定窗口报告哈希和自然发布观测。诊断通过现有
4191 Nginx 网关，分别使用 prc 和 lgq 的短期会话，以验证私有来源对 prc 的
403 边界；不绕过主机可见性规则。短期诊断会话已逐一删除并核实不存在，
本次上传的临时二进制已清理，保留具名发布清单与回滚备份。
