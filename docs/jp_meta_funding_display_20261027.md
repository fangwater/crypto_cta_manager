# jp-meta Funding 刷新闪烁修复

2026-10-27 按操作员要求，将 el01 已验证的 Funding 显示修复同步发布到
jp-meta。修复代码为 `6ba6577`：NAV 更新期间保留上一份有效 Funding，新结果
完成后替换；暂时未知或计算失败不清空同一锚点、同一类型的已有金额。保留实际
统计时间与覆盖状态，单独标识后台更新。首次未知仍为空，持仓锚点变化后的未知
结果不能沿用旧锚点金额。费用计算、每日保存和实际/估算选择规则不变。

发布目录为
`/home/ubuntu/crypto_cta_manager/web-releases/funding-display-20261027T063712Z`。
后端 SHA-256 为
`741bd80a5ccbc7dfcc27e3ba9e8223e674ce60e06740c7a11ca8dd1c68bdaca0`，
与 el01 修复版本完全一致。前端四份文件也与 el01 同一构建逐一校验一致。
本次重新执行 release 构建确认产物，复用该代码在 el01 发布前已通过的格式、
检查、回归测试、全部库测试（432 通过、5 跳过）及前端构建结果。

只替换 Manager 后端、前端并重启 Manager。主机 TOML 的完整 SHA-256 仍为
`28c876122f811211b29af667a8eed6b032f43ebb3428d6aaab9d841034e5b7da`。
公开费率出口、Kline 预算、实际 Funding 开关与轮询设置、unit 和 Nginx 均不变。
RX01/trade06、RX03/trade07 继续使用实际账单，其余账户使用估算。
发布前确认已有 `restatements` 列，没有执行 SQL DDL 或改动历史数据。
最终 Manager PID `2735398`，CPU affinity `24`，自动重启次数为零。

旧二进制、配置和前端链接保存在
`/home/ubuntu/crypto_cta_manager/backups/funding-display-20261027T063712Z/`。
前端保留旧 assets，供已打开的页面继续加载。精确部署暂存目录已清理。

验证结果：

- 通过现有 Nginx 鉴权接口连续采样 157 秒、53 次，跨 3 个 NAV generation。
  8 个账户首次有值后没有退回空值，观察到 21 个旧金额在新 generation 计算期间
  被保留的实例。该验证检查 API 行为，未运行浏览器。
- 8 个账户的 `timeline`、`account-timeline` 共 16 份固定区间订单 NAV 报告，
  完整哈希与发布前一致。
- 32 份主机、交易、Nginx、Manager unit 配置哈希不变；四张初始持仓快照表、
  21,095 行归属校准和 182 行每日权益审计记录的行数与整表哈希均不变。
- 157 个持续运行的受保护进程身份与 CPU affinity 不变。初始扩展基线的另外
  两个 NAV Manager `sync_history` 进程在观察期间退出，明细保存在核对文件中；
  发布操作仅针对 CTA Manager，没有操作这两个进程。交易、Viz、Config、Nginx
  和其他持续运行服务没有重启。
- Nginx 提供的四份前端资源与发布清单 SHA-256 一致，临时鉴权 session 已删除。
  历史费用覆盖仍可能部分缺失，保留既有覆盖提示与后台补齐。

发布清单、基线与连续采样结果见 `artifacts/jp_meta_funding_display_20261027/`。
