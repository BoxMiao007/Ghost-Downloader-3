# CLAUDE.md

本文件是给 Claude Code / AI 代理快速读取的仓库协作说明。更完整的项目说明见 `AGENTS.md`。

## 项目概览

Ghost-Downloader-3 是基于 PySide6 和 PyQt-Fluent-Widgets 的跨平台多线程下载器，支持 HTTP、FTP、BT、M3U8、Bilibili 等插件式下载。本仓库是 `XiaoYouChR/Ghost-Downloader-3` 的 Fork，包含本 Fork 独有的 Rust HTTP 引擎和浏览器扩展增强。

## 上游同步保护清单

合并上游时，以下内容不得被覆盖、删除或回退：

- `gd3-engine/`：Rust HTTP 下载引擎完整目录
- `app/supports/engine.py`：Rust 引擎可用性检测与 `HttpEngine` 枚举
- `features/http_pack/rust_worker.py`：Rust Worker 薄包装
- `.github/workflows/build-engine.yml`：Rust 引擎 CI 构建
- `.github/workflows/build.yml` 中的 `build-browser-extension` job：浏览器扩展测试、类型检查、构建、打包、上传资产
- `browser_extension/app/src/background/download-suffix-filter.ts`：浏览器下载后缀排除规则
- `browser_extension/app/src/background/popup-settings.ts`：popup 设置保存轻量确认响应
- `browser_extension/app/src/shared/runtime-messages.ts`：runtime message 校验与错误归一化
- `browser_extension/app/tests/download-suffix-filter.test.ts`：后缀过滤测试
- `README.md` / `README_zh.md` 中的 `<!-- FORK NOTICE -->` 段

以下上游文件包含本 Fork 修改，同步冲突时需要手动合并并保留本 Fork 行为：

- `features/http_pack/task.py`、`features/http_pack/pack.py`：HTTP 任务引擎选择
- `app/supports/config.py`：`VERSION` 与 `httpEngine` 配置
- `app/supports/update.py`：Release API 指向本 Fork
- `app/view/pages/setting_page.py`、`app/view/components/add_task_dialog.py`：Rust/Python 引擎选择 UI
- `deploy.py`、`pyproject.toml`、`.gitignore`：Rust 引擎构建与依赖配置
- `browser_extension/app/src/background.ts`：浏览器下载接管前执行后缀排除；设置保存避免等待完整状态
- `browser_extension/app/src/popup/hooks/usePopupBridge.ts`、`browser_extension/app/src/popup/components/SettingsPage.tsx`、`browser_extension/app/src/shared/types.ts`：popup 设置与状态字段

## 版本与 Release

- 本 Fork 版本号格式为 `{上游版本}-{迭代号}`，例如 `3.10.2.1-3`；tag 使用 `v` 前缀，例如 `v3.10.2.1-3`
- `app/supports/config.py` 中的 `VERSION` 必须与 tag 去掉 `v` 后一致
- `deploy.py` 的 `format_pe_version(VERSION)` 会把 Fork 迭代号转换为 Windows PE 可用的最多 4 段数字版本
- Release 必须包含桌面端资产、Rust 引擎相关产物，以及浏览器扩展资产：`Ghost-Downloader-v{VERSION}-BrowserExtension-Chromium.zip`、`Ghost-Downloader-v{VERSION}-BrowserExtension-Firefox.zip`
- 同步上游或改 Release 工作流后，必须确认 `.github/workflows/build.yml` 仍能通过 `browser_extension` 或 `all` 目标构建并上传两个扩展 zip

## 浏览器扩展关键约束

- `interceptDownloads` 是浏览器下载接管总开关；关闭时不执行后缀过滤和接管
- 后缀排除规则只影响浏览器原生下载接管，不改变资源嗅探、手动发送资源或桌面端协议
- 后缀匹配大小写不敏感，用户可输入 `.zip, exe` 或 `zip exe`；无法识别后缀时默认交给 Ghost Downloader 3
- popup 的简单设置保存命令应返回轻量确认 payload，不要等待完整 `buildPopupState()`；完整状态通过后续刷新同步，避免 `message port closed before a response`
- popup runtime message 错误应通过 `browser_extension/app/src/shared/runtime-messages.ts` 归一化，避免直接显示浏览器英文通道错误

## 常用验证命令

```bash
uv sync
uv run python Ghost-Downloader-3.py
uv run python deploy.py

cd gd3-engine && cargo test
uv run python -c "from app.supports.engine import isRustEngineAvailable; print(isRustEngineAvailable())"

cd browser_extension/app && npm run test && npm run typecheck && npm run build
git diff --check
```

## 开发约定

- Python 代码使用 camelCase 变量/方法、PascalCase 类名；日志和用户可见文本优先使用简体中文
- UI 可翻译文本使用 `self.tr("...")`
- 修改既有代码前先阅读上下文注释，逻辑变更时同步更新失效注释
- 不要删除或覆盖本 Fork 独有 Rust 引擎和浏览器扩展增强；不确定时先对照 `AGENTS.md` 的同步清单
