# 2026-10-15 本地大文件归档

按操作员要求整理提交时，曾将 54 份原始诊断响应、构建二进制、部署包、源码
补丁、日志、截图及回滚备份移出工作区，保留在本机
`/home/fanghaizhou/crypto_cta_manager-archives/20261015-precommit/`。
目录内保留原仓库相对路径；这些文件未加入 Git。

[校验清单](local_archive_20261015.json) 记录每个文件的原相对路径、字节数和
SHA-256；移动后已逐一复核。随后按操作员要求清理了 25 份可重建、重复或
临时验收文件，保留 29 份证据文件（约 64.1 MiB）。清单的 `status` 明确标记
`retained` 或 `removed`，删除记录保留原校验值、清理原因及重复文件的保留路径。

旧版监控二进制、原始 journal 压缩备份、SQL 快照、源码补丁和不重复的事实
回放数据继续保留；剩余文件已重新核验 SHA-256。原始回滚压缩包的全部 11 份
内容与保留的展开文件逐一匹配后，才删除重复压缩包。

本次同时清理了工作区的一次性诊断程序
`artifacts/jp_meta_refresh_failure_20261014/readonly_manager_archive_probe`
及 `frontend/dist` 构建输出，共删除 30 个文件，释放 235.5 MiB。

首次归档涉及以下证据目录；当前保留状态以清单为准，其源码、摘要、验证结果
和小型部署记录仍在仓库内：

- `el01_bahll_30d_20261015`：大型 API 原始响应。
- `jp_meta_manager_cpu_20261014`：线程和配置验证压缩包。
- `jp_meta_rx01_equity_20261015`：新旧监控二进制、原始 journal 和备份压缩包。
- `jp_meta_rx01_initial_snapshot_20261015`：构建二进制、前端部署包及大型回放输入和输出。
- `position_creation_el01_20261014T093305Z`：部署源码补丁和临时验收截图。
- `position_owner_el01_20261015`：部署源码补丁。
