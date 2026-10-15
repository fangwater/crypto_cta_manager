# 2026-10-15 本地大文件归档

按操作员要求整理提交时，将 54 份原始诊断响应、构建二进制、部署包、源码
补丁、日志、截图及回滚备份移出工作区，保留在本机
`/home/fanghaizhou/crypto_cta_manager-archives/20261015-precommit/`。
目录内保留原仓库相对路径；这些文件未加入 Git，也未删除。

[校验清单](local_archive_20261015.json) 记录每个文件的原相对路径、字节数和
SHA-256；移动后已逐一复核。归档涉及以下证据目录，其源码、摘要、验证结果
和小型部署记录仍在仓库内：

- `el01_bahll_30d_20261015`：大型 API 原始响应。
- `jp_meta_manager_cpu_20261014`：线程和配置验证压缩包。
- `jp_meta_rx01_equity_20261015`：新旧监控二进制、原始 journal 和备份压缩包。
- `jp_meta_rx01_initial_snapshot_20261015`：构建二进制、前端部署包及大型回放输入和输出。
- `position_creation_el01_20261014T093305Z`：部署源码补丁和临时验收截图。
- `position_owner_el01_20261015`：部署源码补丁。
