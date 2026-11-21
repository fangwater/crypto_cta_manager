# 国内期货 PM2 部署

Manager 与同步程序分别运行。两者使用 `mode = "cn_futures"`，共用本机接收配置；
只有同步程序写入专用副本，Manager 通过独立 secondary 读取。数据库为独立的
`cn_futures_cta_manager`，不复用 Crypto 或行情数据库的表。主机凭据文件保持 0600，
不提交 Git，也不把凭据写入 PM2 配置或命令行。

## 本机

在现有 checkout 下准备 `releases/<release>/bin/{cn_futures_web,sync-server}` 与
`releases/<release>/frontend/`；将 `run/current` 原子指向已校验的 release。
维护 `config/sync-server-collect.toml`，来源示例见 `config/sync-server-collect.example.toml`。
主机文件 `~/.config/cn-futures-cta-manager/host.env` 只定义数据库 URL 和同步令牌，
变量名分别与 TOML 的 `database.url_env`、`sources.sync.token_env` 一致。

`scripts/run_native_service.sh` 在子进程中加载环境文件，PM2 只保存文件路径和启动角色。
默认开放文件数 65536，Tokio／Rayon 各 2 线程，SIGINT 后最多等待 60 秒退出。
首次初始化仅对新建、空数据库显式运行 `cn_futures_web --init-db`；后续重启不执行 DDL。

```bash
pm2 start deploy/cn_futures/ecosystem.config.cjs --only cn-futures-sync-tunnel
# 首次先用相同环境文件执行 sync-server pull --once，确认提交完整水位。
pm2 start deploy/cn_futures/ecosystem.config.cjs --only cn-futures-sync,cn-futures-manager
pm2 save
```

`cn-futures-sync-tunnel` 使用已有 `sh_dev` SSH 别名，将本机 `127.0.0.1:18212`
转发至云端 loopback 导出器。可通过 `CN_FUTURES_SYNC_SSH_HOST` 更换 SSH 别名。
同一连接将本机 Manager 反向映射至云端 `127.0.0.1:18211`，由云端 Nginx 代理
`/manager/`；片段为 `deploy/cn_futures/nginx-manager.conf`，加入既有 server 后先
`nginx -t`，通过后 reload。保留 URL 前缀，不改变现有 CTA 或 Viz 的路由。
它使用心跳和 `ExitOnForwardFailure`，失败时由 PM2 重启。增加其他云端时显式增加
独立的隧道端口、source_id 与导出 URL，不共享身份或副本。

Manager 的 `--frontend-root` 提供已构建网页和同源 API，总览地址为
`http://127.0.0.1:18211/manager/workspace/`。国内期货沿用 Crypto CTA 页面和交互，后端按相同页面接口返回账户数据。CNY、手数与原生合约字段在 CN 构建中适配。发布时校验对外静态文件与本地 CN 构建的 SHA-256。每条授权来源在 `/manager/accounts/` 独立显示，详情链接以
`source` 参数保留账户身份。网页可匿名加载，数据 API 保持登录和来源权限检查。
`execution_monitor_urls` 按 Manager 来源配置执行监控入口。国内 Viz 使用 URL 的 `source` 参数预选对应柜台。
首次初始化管理员后再开放云端入口。部署可生成强随机初始密码并仅存入本机 0600
的私有登录文件，操作者自行读取；不在日志或 Git 中保存密码。
通过既有反向代理或 SSH 转发访问；API 保持 loopback。没有启动 Vite 开发服务器。

替换已校验的 `run/current` 后，只重启网页进程使用
`scripts/restart_native_manager.sh --execute`；省略参数先查看计划。
脚本核对现有进程的工作目录并备份日志，不重启同步器、隧道或柜台观察进程。

## 云端

云端仅安装 `sync-server`、launcher、PM2 配置及独立导出配置，路径与执行环境分离。
执行库以 secondary 只读打开，云端同步日志、secondary 写入本组件自己的目录。
构建在当前 checkout 完成，上传临时文件后校验 SHA-256，再原子安装；不在云端编译。

```bash
pm2 start deploy/cn_futures/ecosystem.config.cjs --only cn-futures-exporter
pm2 save
```

单独启停这四个名称中的目标进程，不执行 PM2 的全局 restart/delete，也不启停柜台进程。
CTP 源进程已停止或失败时，仍可同步其已持久化历史；同步成功不代表柜台在线。
验收应同时记录 PM2 进程、同步水位和源捕获时间、真实库订单／成交／状态统计及
登录边界。部署前后核对执行配置和二进制哈希、既有服务 PID。
