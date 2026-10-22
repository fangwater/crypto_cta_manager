# jp-meta Manager 前端目录修复 — 2026-10-21

13:30 UTC 实测公网 `/manager/` 返回 Nginx HTTP 500。
Manager 服务仍为 active，PID `1049854`，`/api/auth/status` 返回 200。
Nginx 错误为回退到 `/manager/index.html` 时发生内部重定向循环。

当前前端发布 `20261021T122000ZAllocation` 将 `index.html`、`favicon.svg`
和 `assets/` 直接放在发布目录根层，缺少 Nginx 所需的 `manager/` 子目录。
现有 `scripts/deploy_host.sh` 使用正确布局；本次问题来自该次单独发布的目录结构。

13:32:32 UTC 将同一份前端文件复制到新发布的 `manager/` 子目录，再原子切换
`webroot`，新发布为：

`/home/ubuntu/crypto_cta_manager/web-releases/20261021T133232ZLayoutFix`

四个前端文件的 SHA-256 全部保持不变。原发布目录完整保留；回退指向、文件哈希、
进程检查和验证结果保存于远端受限目录：

`/home/ubuntu/crypto_cta_manager/backups/frontend_layout_20261021T133232Z/`

修复后通过主机本地网关及公网验证：首页、workspace、accounts、config 页面均返回
200；JS、CSS、favicon 和 index.html 下载内容与原发布文件哈希一致，响应类型正确；
匿名登录状态接口返回正常的未登录状态。未执行浏览器渲染检查。

本次仅调整前端发布布局和 webroot 指向，没有重启服务或修改 Nginx 配置、后端二进制、
数据库、Exec 数据与交易配置。检查的 129 个相关进程 PID/启动标识全部保持不变。
