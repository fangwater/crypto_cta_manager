# el01 目标推送 HTTP 500 修复，2026-10-21

操作员报告 `USDC_C40V6D3PosV5_filter` 在 07:05 UTC 推送 25 个目标时，
`POST /manager/api/catalog/position-strategies` 连续三次返回 HTTP 500。
这是 API 的数据库错误，与此前前端目录造成的页面 500 是不同问题。

## 原因

el01 当前 Manager 读取订单模板时查询 `cta_order_strategies.grid`，但既有数据库
没有该列，日志为 `failed to list order strategies: column "grid" does not exist`。
07:05 和修复前的请求均命中此错误，处理在保存目标、归档和下发之前终止。
同表也缺少订单模板所有权字段 `created_by_user_id`，订单模板列表接口同样需要它。

当前二进制与数据库结构未同步：发布前应执行的两份显式维护 SQL 尚未在 el01 执行。
普通健康检查和 NAV 查询并不能覆盖订单模板读取及目标推送链路。

## 处理

2026-10-21 07:35:13 UTC，在 el01 的 `crypto_cta_manager` 数据库完成显式维护：

- 备份 `cta_order_strategies` 为可供 `pg_restore` 使用的 custom dump，并保存完整
  schema；验证备份可读取。备份仅留在远端
  `/home/el01/crypto_cta_manager/backups/publish-schema-20261021T073512Z/`。
- 在一个事务内执行既有 `scripts/apply_grid_order_schema.sql` 和
  `scripts/apply_order_strategy_ownership.sql`，设置锁等待上限 3 秒、语句上限 15 秒。
- 添加 Grid JSON 默认值及约束、补齐算法允许值、添加可空所有者外键。
  六个原有模板均为 `batch`，原有参数、时间戳和所有权语义保持不变。
- 没有重放旧信号；由正常运行的发布方提交新目标。

没有更换二进制、修改配置或重启任何服务。Manager PID 保持 `3422779`。
维修前后原有模板字段、账户绑定和四张持仓快照表的行数/内容哈希一致；
16 份配置哈希及抽取核对的 60 个运行进程启动标识一致。
新增字段不启用 Grid 策略，也不创建或修改任何账户绑定。

## 核验

目标策略正常推送于 07:35:14.385369 UTC 保存，trade14 运行配置于
07:35:14.433266 UTC 更新。Manager 成功下发日志和确认 Redis 写入后的重载通知
均出现在 07:35:14.434 UTC。

通过原有 Nginx 网关只读核验：

- Manager 健康状态 `ok`，14 个启用来源，刷新错误为空。
- 订单模板列表 HTTP 200，返回六个模板。
- trade14 Exec Config HTTP 200，策略存在；25 个目标数量全部等于当前模板数量
  × 原有份额 25，逐目标 signal 也全部一致。
- 截至 07:37:35 UTC，没有新的缺 `grid` 错误，也没有该策略的下发失败日志。
  修复后观测到目标推送 HTTP 200 共 8 次、502 共 112 次、499 共 3 次，500 为零。
- 两次只读验证使用的临时登录会话均通过 `finally` 删除；凭据没有输出或写入证据。

证据及执行脚本保存在 `artifacts/el01_publish_schema_20261021/`。

## 独立存在的 trade05 绑定错误

恢复目标读取后，`rbf_small`、`rbf_big`、`funding` 暴露另一类 HTTP 502：
`source_id is disabled: binance_exec_trade05`。这些策略仍有指向停用 trade05 的
正份额绑定；启用账户的下发继续成功，但该停用绑定使整个请求返回部分失败。
另有两个 CTA 策略也保留该账户的正份额绑定。

本次没有启用 trade05，也没有更改、清零或删除这些绑定。这不影响本次
`USDC_C40V6D3PosV5_filter` 在 trade14 的恢复；不能把 el01 所有策略都描述为无错误。

后续发布需在切换二进制前核对所需 schema，按既有说明显式维护。发布验证还应覆盖
订单模板接口及自然目标推送的成功日志，不能只依据 health/NAV 返回 200。
不得把 schema 更新加入应用启动流程。
