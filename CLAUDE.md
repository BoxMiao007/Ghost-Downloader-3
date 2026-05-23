# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Ghost-Downloader-3 is a cross-platform multithreaded download manager built with PySide6 and PyQt-Fluent-Widgets (Fluent Design). It supports HTTP, FTP, BitTorrent, M3U8, and Bilibili video downloads via a plugin system. The async engine uses winloop (Windows) / uvloop (Linux/macOS).

## Fork 与上游的关系

本仓库是 [XiaoYouChR/Ghost-Downloader-3](https://github.com/XiaoYouChR/Ghost-Downloader-3) 的 Fork。

**上游同步注意事项：** 合并上游更新时，以下文件/目录是本 Fork 独有的，**不得被上游覆盖或删除**：

### 本 Fork 独有文件（同步时保护）

| 路径 | 说明 |
|------|------|
| `gd3-engine/` | Rust HTTP 下载引擎（整个目录） |
| `app/supports/engine.py` | 引擎可用性检测、HttpEngine 枚举 |
| `features/http_pack/rust_worker.py` | Rust Worker 薄包装 |
| `.github/workflows/build-engine.yml` | Rust 引擎 CI 构建 |
| `README.md` 中 `<!-- FORK NOTICE -->` 段 | Fork 差异说明 |
| `README_zh.md` 中 `<!-- FORK NOTICE -->` 段 | Fork 差异说明（中文） |

### 本 Fork 修改的上游文件（同步时需手动合并）

| 路径 | 修改内容 |
|------|----------|
| `features/http_pack/task.py` | 新增 `engine` 字段，`__post_init__` 动态选择 workerType |
| `features/http_pack/pack.py` | `parse()` 中注入 `engine` 到 metadata |
| `app/supports/config.py` | 新增 `httpEngine` OptionsConfigItem |
| `app/supports/update.py` | `RELEASE_API_URL` 指向 Fork 仓库 |
| `app/view/pages/setting_page.py` | 新增引擎选择 ComboBoxSettingCard |
| `app/view/components/add_task_dialog.py` | 新增 `EngineSelectCard` 每任务引擎选择 |
| `deploy.py` | `COMMON_INCLUDE_PACKAGES` 添加 `"gd3_engine"`；`PE_VERSION` 处理版本号格式 |
| `pyproject.toml` | 新增 `[project.optional-dependencies] rust` 和 `[tool.uv.sources]` |
| `.gitignore` | 新增 Rust 构建产物、Claude Code 等忽略规则 |

### 上游同步流程

```bash
git fetch upstream
git merge upstream/main
# 如有冲突，优先保留上表中本 Fork 的修改
# 合并后验证：
cd gd3-engine && cargo test
uv run python -c "from app.supports.engine import isRustEngineAvailable; print(isRustEngineAvailable())"
```

### 版本号规则

- 本 Fork 版本号格式：`{上游版本}-{迭代号}`，例如 `v3.9-1`、`v3.9-2`
- 前缀版本号（如 `3.9`）仅在同步上游时更新，跟随上游版本
- 后缀迭代号（如 `-1`、`-2`）用于本 Fork 自身的功能迭代
- `app/supports/config.py` 中的 `VERSION` 保持与 tag 一致（如 `"3.9-1"`）
- `deploy.py` 中 `PE_VERSION = VERSION.replace("-", ".")` 用于 Nuitka/macOS 版本号字段（不接受连字符）

### Release 发布说明排版规范

发布说明采用**中文在上、英文在下**的双语结构，用双分隔线 `---\n---` 隔开：

```markdown
## 🚀 新特性

- **功能名称** — 简要描述

## 🐛 修复

- **问题描述** — 修复内容

## 📦 打包改进

- 改进内容

## 🔧 其他

- 其他变更

---

### Rust 引擎可用性

| 平台 | 架构 | Rust 引擎 |
|------|:----:|:---------:|
| Windows | x86_64 | ✅ 内置 |
| Windows | ARM64 | ⚠️ 需自行构建 |
| macOS | ARM64 | ✅ 内置 |
| macOS | x86_64 | ✅ 内置 |
| Linux | x86_64 | ✅ 内置 |
| Linux | ARM64 | ✅ 内置 |

---
---

## 🚀 What's New

- **Feature name** — Brief description

## 🐛 Fixes

- **Issue description** — Fix details

## 📦 Packaging

- Improvement details

## 🔧 Other

- Other changes

---

### Rust Engine Availability

（同上表英文版）
```

## Rust HTTP 引擎 (`gd3-engine/`)

可选的高性能 HTTP 下载引擎，通过 PyO3 暴露为 Python 扩展模块 `gd3_engine`。

### 架构

```
gd3-engine/src/
├── lib.rs          # PyO3 模块入口，暴露 Python API
├── engine.rs       # 下载编排器（tokio runtime + DownloadHandle）
├── scheduler.rs    # 自适应调度器（探测→估算→扩缩→窃取）
├── connection.rs   # 单连接下载（重试、退避、流式读取）
├── probe.rs        # HTTP 探测（Range 支持、文件名解析）
├── writer.rs       # 磁盘写入（跨平台 pwrite）
├── resume.rs       # 断点续传（.ghd 读取 + .ghdx 格式）
├── progress.rs     # 进度报告（AtomicU64 共享内存）
├── speed_limit.rs  # 令牌桶限速
├── config.rs       # DownloadConfig 配置结构体
└── error.rs        # 错误类型
```

### 开发命令

```bash
# 构建并安装到当前 venv
cd gd3-engine && maturin develop --release

# 运行测试（28 个单元测试）
cargo test

# 仅检查编译
cargo check
```

### Python API

```python
import gd3_engine

# 探测
result = gd3_engine.probe(url, headers={}, proxies={}, verify_ssl=False)
# -> ProbeResult { file_size, supports_range, file_name, final_url }

# 下载
config = gd3_engine.DownloadConfig(url, output_path, ...)
handle = gd3_engine.start_download(config)
# -> DownloadHandle { .progress, .pause(), .cancel(), .set_speed_limit(n) }

# 进度
p = handle.progress
# -> DownloadProgress { .received_bytes, .total_bytes, .speed, .percent, .state, .error }
```

### 引擎切换机制

1. `app/supports/config.py` 中 `cfg.httpEngine` 控制全局默认（"python" / "rust"）
2. `features/http_pack/pack.py` 的 `parse()` 将引擎选择写入 `task.metadata["engine"]`
3. `features/http_pack/task.py` 的 `HttpTaskStage.__post_init__` 根据 `self.engine` 字段动态设置 `workerType`
4. 若 Rust 引擎不可用，自动回退到 Python 引擎（`HttpWorker`）

## Development Commands

```bash
# 安装依赖（使用 uv）
uv sync

# 运行应用
uv run python Ghost-Downloader-3.py
# 静默启动（最小化到托盘）
uv run python Ghost-Downloader-3.py --silence

# 国际化：提取翻译字符串 → 编译 .qm → 重建 resources.py
uv run python sync_i18n_res.py

# 构建发布包（Nuitka 编译 + 复制 FeaturePacks）
uv run python deploy.py
```

## Architecture

### 启动流程

入口 `Ghost-Downloader-3.py` 按顺序：配置日志 → 加载用户配置 → 创建 SingletonApplication → 启动 CoreService 线程 → 构建 MainWindow → 加载 FeaturePacks → 恢复持久化任务。

### 核心服务层 (`app/services/`)

- **CoreService** (`core_service.py`): 运行在独立 QThread 中的 asyncio 事件循环。管理任务调度（等待队列 + 并发槽位限制）、异步协程执行、桌面通知。通过 `coreService` 单例访问。
- **FeatureService** (`feature_service.py`): 发现、加载、管理 FeaturePack 插件。通过 URL 匹配分发到对应插件。通过 `featureService` 单例访问。
- **CategoryService** (`category_service.py`): 下载分类规则管理，根据文件扩展名/URL 模式自动分配下载目录。通过 `categoryService` 单例访问。
- **BrowserService** (`browser_service.py`): WebSocket 服务器，与浏览器扩展通信（配对认证、任务创建、状态同步）。

### 插件系统 (`features/`)

每个插件是一个目录，包含 `manifest.toml`（声明入口文件和依赖）和一个继承 `FeaturePack` 的类。

```
features/
├── http_pack/       # HTTP 多线程下载（默认）
├── ftp_pack/        # FTP 下载
├── bittorrent_pack/ # BT/磁力链接
├── m3u8_pack/       # HLS 流媒体
├── bili_pack/       # Bilibili 视频
├── ffmpeg_pack/     # FFmpeg 合并
├── extract_pack/    # 解压缩
└── github_pack/     # GitHub Release
```

**FeaturePack 接口** (`app/bases/interfaces.py`):
- `matches(url)` — URL 是否由此插件处理
- `parse(payload)` → `Task` — 解析 URL 创建任务
- `taskCard()` / `resultCard()` — 自定义 UI 卡片
- `config` — 可选的 `PackConfig` 子类，注册设置项

### 任务模型 (`app/bases/models.py`)

- **Task**: 下载任务，包含多个 Stage，支持序列化/反序列化（orjson）
- **TaskStage**: 任务的一个执行阶段（如单个文件分片），通过 `_registry` 实现多态反序列化
- **Worker**: 执行 Stage 的异步工作单元（`async run()`）
- **TaskStatus**: WAITING → RUNNING → COMPLETED/FAILED/PAUSED

Task 执行流程：`CoreService.createTask()` → 槽位调度 → `Task.run()` → 遍历 `pendingStages()` → 实例化 `stage.workerType(stage)` → `worker.run()`

### 持久化 (`app/supports/recorder.py`)

`TaskRecorder` 将任务序列化为 JSONL 格式写入 `Memory.log`，应用启动时恢复未完成任务。

### 支撑层 (`app/supports/`)

- `config.py` — 基于 qfluentwidgets QConfig 的配置系统，插件通过 `PackConfig` 子类注册配置项
- `signal_bus.py` — Qt Signal 总线（异常广播、窗口显示）
- `paths.py` — 应用数据目录
- `application.py` — 单实例应用（防止多开）

### 视图层 (`app/view/`)

- `windows/main_window.py` — 主窗口
- `pages/` — 任务页、设置页
- `components/` — 任务卡片、对话框、系统托盘等 UI 组件

## Conventions

- 命名风格：camelCase（变量、方法），PascalCase（类）
- 日志使用 `loguru`，日志消息使用中文
- 所有可翻译的 UI 文本使用 `self.tr("...")`，源语言为中文
- 支持的语言列表在 `sync_i18n_res.py` 的 `RUNTIME_LANGUAGES` 中定义
- 序列化使用 `orjson`（高性能 JSON）
- 配置文件存储在平台标准应用数据目录下

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **Ghost-Downloader-3** (4018 symbols, 9093 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> If any GitNexus tool warns the index is stale, run `npx gitnexus analyze` in terminal first.

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `gitnexus_impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `gitnexus_detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `gitnexus_query({query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `gitnexus_context({name: "symbolName"})`.

## Never Do

- NEVER edit a function, class, or method without first running `gitnexus_impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `gitnexus_rename` which understands the call graph.
- NEVER commit changes without running `gitnexus_detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/Ghost-Downloader-3/context` | Codebase overview, check index freshness |
| `gitnexus://repo/Ghost-Downloader-3/clusters` | All functional areas |
| `gitnexus://repo/Ghost-Downloader-3/processes` | All execution flows |
| `gitnexus://repo/Ghost-Downloader-3/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->
