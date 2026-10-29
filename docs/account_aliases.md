# 账户别名与授权

2026-10-28 已部署到 jp-meta，发布目录为
`web-releases/account-alias-20261028T125243Z`。只重启 Manager，CPU 绑定保持 24；
没有修改账户可见授权、主机 TOML 或交易服务。

账户页 `/manager/accounts/` 的账户名称支持双击编辑，也可点击名称旁的编辑按钮。
Enter 或保存按钮提交，Esc 或取消按钮退出。清空后恢复 TOML 的 `alias`，
未配置 TOML 别名时使用 `account`。别名去除首尾空白，最多 200 个字符，
拒绝控制字符；修改失败时保留输入并显示错误。

`cta_order_sources.display_alias` 只保存 Manager 展示覆盖，账户注册不会覆盖它。
账户页、配置页、净值/成本页的账户选择和本地账户监控概览使用最新展示名。
`source_id`、绑定、历史归档、Exec 配置与交易身份保持原值。

所有登录用户都可修改自己可见账户的别名，无须单独授权，也不会因此获得绑定、
费率、杠杆或授权的配置权限。`dashboard.source_viewers` 仍限制账户可见范围。

接口：`PUT /api/catalog/accounts/{source_id}/alias`，JSON 为
`{"alias":"新的展示名"}`；`{"alias":null}` 或空白字符串恢复配置名称。
必须使用已登录会话，后端检查账户可见性。
`GET /api/dashboard` 返回每个账户的 `display_alias` 和会话的 `can_edit_alias`。
账户授权接口不增加新的权限字段。

现有 jp-meta 数据库需要显式维护，部署前先备份 `cta_order_sources`，再执行：

1. `scripts/apply_account_alias_schema.sql`：添加别名列。

该脚本可重复执行；不会新增任何账户查看权限。普通启动不执行 DDL。
新数据库的 `migrations/schema.sql` 已包含当前结构。

部署前的 Manager 程序、前端指向和账户目录备份位于主机
`~/crypto_cta_manager/backups/account-alias-20261028T125243Z/`。
后端 SHA-256 为 `640962c2a7d4aeeaec489c5b157c703f6ced0738f756d91bf61772c0a3d8ce91`。
全量 Rust 测试、前端构建及 PostgreSQL 16 迁移和权限测试通过。线上使用 `lgq`
对仅查看账户验证了改名、账户及报表名称更新、隐藏账户拒绝和配置写入拒绝；
测试名称已恢复。Chromium 桌面双击保存、手机编辑取消及无横向溢出检查通过。
