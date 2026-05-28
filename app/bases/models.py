import asyncio
from dataclasses import dataclass, field, fields as dataclass_fields, is_dataclass
from enum import auto, IntEnum
from pathlib import Path
from time import time_ns
from typing import ClassVar, Dict, Type, Any, TYPE_CHECKING, Iterable
from uuid import uuid4

from PySide6.QtCore import QCoreApplication
from loguru import logger
from orjson import loads, dumps
from qfluentwidgets import SettingCard

from app.supports.config import cfg, ConfigItem
from app.supports.utils import removePath, toSafeFilename


if TYPE_CHECKING:
    from app.bases.interfaces import Worker
    from app.view.pages.setting_page import SettingPage
    from PySide6.QtWidgets import QWidget


class TaskStatus(IntEnum):
    """任务和阶段共享的生命周期状态。

    Task.updateStatus() 会从所有 Stage 聚合出 Task 状态，因此新增状态时
    必须同时审视阶段聚合、持久化反序列化和 UI 展示逻辑。
    """

    WAITING = auto()
    RUNNING = auto()
    PAUSED = auto()
    COMPLETED = auto()
    FAILED = auto()


class SpecialFileSize(IntEnum):
    """下载探测无法得到普通正整数大小时使用的哨兵值。"""

    NOT_SUPPORTED = -1
    UNKNOWN = 0


def _toSerializable(obj: Any) -> Any:
    """递归转换为 orjson 可写入的结构。

    dataclass 字段只有 repr=True 才会被持久化；这让 workerType、stageType
    等运行期对象不会进入 Memory.log。修改字段 repr 时要同步确认任务恢复
    是否仍能依靠 registry 找回正确子类。
    """
    if isinstance(obj, TaskStatus):
        return obj.name
    if isinstance(obj, Path):
        return str(obj)
    if isinstance(obj, (TaskStage, Task)):
        result = {
            f.name: _toSerializable(getattr(obj, f.name))
            for f in dataclass_fields(obj) if f.repr
        }
        baseName = "TaskStage" if isinstance(obj, TaskStage) else "Task"
        if type(obj).__name__ != baseName:
            result["type"] = type(obj).__name__
        return result
    if is_dataclass(obj):
        return {
            f.name: _toSerializable(getattr(obj, f.name))
            for f in dataclass_fields(obj) if f.repr
        }
    if isinstance(obj, list):
        return [_toSerializable(item) for item in obj]
    if isinstance(obj, dict):
        return {k: _toSerializable(v) for k, v in obj.items()}
    return obj


def _filterProperty(cls: type, obj: dict[str, Any]) -> dict[str, Any]:
    """仅保留目标 dataclass 构造函数能接收的字段。

    任务记录会跨版本保存在用户目录中。这里丢弃未知字段，是为了让新版应用
    尽量能读取旧记录，也让删除字段后的记录恢复不至于直接失败。
    """
    allowed = {field.name for field in dataclass_fields(cls) if field.init}
    for klass in cls.__mro__:
        for name, val in vars(klass).items():
            if isinstance(val, property):
                allowed.discard(name)
    return {key: value for key, value in obj.items() if key in allowed}


@dataclass(kw_only=True)
class TaskFile:
    """多文件任务中的一个可选文件条目。

    index 是插件内稳定编号，Task.setSelection() 依赖它把 UI 选择映射回
    Stage；relativePath 必须是相对路径，避免恢复任务时越过下载目录。
    """

    index: int
    relativePath: str
    size: int = 0
    selected: bool = True
    downloadedBytes: int = 0
    completed: bool = False


@dataclass(kw_only=True)
class TaskStage:
    """任务的最小执行单元。

    每个子类会自动登记到 _registry，反序列化时通过 type 字段恢复具体
    Stage 类型。Stage 只描述进度、状态和执行参数；真正的异步执行由
    workerType 指向的 Worker 完成。
    """

    _registry: ClassVar[Dict[str, Type["TaskStage"]]] = {}
    workerType: ClassVar[Type["Worker"]]
    canPause: ClassVar[bool] = True

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        TaskStage._registry[cls.__name__] = cls

    stageIndex: int
    stageId: str = field(default_factory=lambda: f"stg_{uuid4().hex}")
    status: TaskStatus = TaskStatus.WAITING
    progress: float = 0
    receivedBytes: int = 0
    speed: int = 0
    error: str = ""

    def _bindTask(self, task: "Task"):
        self._task = task

    @property
    def task(self) -> "Task":
        """返回所属 Task。

        Stage 必须通过 Task.addStage() 或 Task.__post_init__ 绑定后才能访问
        task；独立构造的 Stage 访问该属性会暴露调用方的生命周期错误。
        """
        return self._task

    def setStatus(self, status: TaskStatus, sync: bool = True):
        """更新阶段状态，并按需回写父任务聚合状态。

        sync=False 用于批量修改多个阶段，避免每次阶段变更都重复聚合任务状态。
        """
        self.status = status
        if status == TaskStatus.COMPLETED:
            self.progress = 100
            self.speed = 0
            self.error = ""
        elif status in {TaskStatus.WAITING, TaskStatus.PAUSED}:
            self.speed = 0
            self.error = ""
        elif status == TaskStatus.FAILED:
            self.speed = 0

        if sync and hasattr(self, "_task"):
            self._task.updateStatus()

    def setError(self, error: Any, sync: bool = True):
        """把任意异常或错误对象记录为阶段失败原因。"""
        message = repr(error).strip() if error is not None else ""
        self.error = message
        self.setStatus(TaskStatus.FAILED, sync=sync)

    def reset(self, sync: bool = True):
        """重置阶段进度以便重新调度。"""
        self.status = TaskStatus.WAITING
        self.progress = 0
        self.receivedBytes = 0
        self.speed = 0
        self.error = ""

        if sync and hasattr(self, "_task"):
            self._task.updateStatus()

    def cleanup(self):
        """Remove per-stage temporary artifacts. Subclasses override."""
        pass

    @classmethod
    def fromFile(cls, file: TaskFile, task: "Task") -> "TaskStage":
        raise NotImplementedError

    def serialize(self) -> bytes:
        """序列化为任务记录使用的 JSON bytes。"""
        return dumps(_toSerializable(self))

    @classmethod
    def deserialize(cls, data: Any) -> "TaskStage":
        """从任务记录恢复 Stage。

        这里依赖子类名作为 type 标识，重命名 Stage 类会影响用户历史任务恢复；
        如需重命名，应保留兼容映射或迁移 Memory.log。
        """
        if isinstance(data, (bytes, bytearray, str)):
            obj = loads(data)
        else:
            obj = data

        typeName = obj.pop("type", None)
        stageCls = TaskStage._registry.get(typeName, cls) if isinstance(typeName, str) else cls

        if "status" in obj and isinstance(obj["status"], str):
            obj["status"] = TaskStatus[obj["status"]]
        if "path" in obj and isinstance(obj["path"], str):
            obj["path"] = Path(obj["path"])

        return stageCls(**_filterProperty(stageCls, obj))


@dataclass(kw_only=True, eq=False)
class Task:
    """下载任务聚合根。

    Task 是 UI、持久化和 CoreService 调度共享的稳定对象。它可以包含一个
    或多个 Stage：普通 HTTP 通常只有一个 Stage，Bilibili/FFmpeg 等组合任务
    会用多个 Stage 串行表达下载、合并或安装步骤。
    """

    _registry: ClassVar[Dict[str, Type["Task"]]] = {}
    supportsEdit: ClassVar[bool] = False

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        Task._registry[cls.__name__] = cls

    title: str
    url: str
    packId: str
    taskId: str = field(default_factory=lambda: f"tsk_{uuid4().hex}")
    status: TaskStatus = TaskStatus.WAITING
    stages: list[TaskStage] = field(default_factory=list)
    createdAt: int = field(default_factory=lambda: int(time_ns()))
    path: Path = field(default_factory=lambda: Path(cfg.downloadFolder.value))
    fileSize: int = 0
    files: list[TaskFile] | None = None
    category: str = ""
    usesSlot: bool = True
    stageType: Type[TaskStage] | None = field(default=None, repr=False)

    @property
    def outputFolder(self) -> str:
        """任务默认输出目录。

        单文件任务可能通过 Stage.outputFile 指向具体文件；清理逻辑会同时考虑
        outputFolder 和每个 Stage 暴露的 outputFile。
        """
        return str(self.path / self.title)

    @property
    def canPause(self) -> bool:
        for stage in self.stages:
            if stage.status == TaskStatus.RUNNING:
                return stage.canPause
        return True

    @property
    def lastError(self) -> str:
        for stage in reversed(self.stages):
            if stage.status == TaskStatus.FAILED and stage.error:
                return stage.error
        for stage in reversed(self.stages):
            if stage.error:
                return stage.error
        return ""

    def __post_init__(self):
        """完成任务构造后的规范化和反向绑定。

        反序列化路径也会进入这里，所以不要放置会改变历史任务语义的探测或
        网络操作；只能做文件名清洗、Stage 绑定和分类补全这类确定性操作。
        """
        self.title = toSafeFilename(self.title, fallback="download")
        for stage in self.stages:
            stage._bindTask(self)
        self.updateStatus()

        if not self.category:
            from app.services.category_service import categoryService

            self.category = categoryService.categoryOf(self)

    def setTitle(self, title: str):
        self.title = toSafeFilename(title, fallback=self.title or "download")

    def currentSnapshot(self) -> tuple[float, int, int]:
        """返回 UI 刷新所需的聚合进度、速度和已接收字节数。"""
        if not self.stages:
            return 0.0, 0, 0

        progress = 0.0
        speed = 0
        receivedBytes = 0
        for stage in self.stages:
            progress += stage.progress
            speed += stage.speed
            receivedBytes += stage.receivedBytes

        return progress / len(self.stages), speed, receivedBytes

    def addStage(self, stage: TaskStage):
        stage._bindTask(self)
        self.stages.append(stage)
        self.updateStatus()

    def removeStage(self, stage: TaskStage):
        self.stages.remove(stage)
        self.updateStatus()

    def updateStatus(self) -> TaskStatus:
        """根据所有阶段状态重新计算任务状态。

        FAILED 优先级最高，COMPLETED 要求所有阶段完成；混合 WAITING/PAUSED
        会回到 WAITING，方便 CoreService 后续继续调度未完成阶段。
        """
        if not self.stages:
            return self.status

        statuses = [stage.status for stage in self.stages]
        if any(s == TaskStatus.FAILED for s in statuses):
            self.status = TaskStatus.FAILED
        elif all(s == TaskStatus.COMPLETED for s in statuses):
            self.status = TaskStatus.COMPLETED
        elif any(s == TaskStatus.RUNNING for s in statuses):
            self.status = TaskStatus.RUNNING
        elif all(s == TaskStatus.PAUSED for s in statuses):
            self.status = TaskStatus.PAUSED
        else:
            self.status = TaskStatus.WAITING

        return self.status

    def setStatus(self, status: TaskStatus) -> TaskStatus:
        """批量设置未完成阶段状态。

        从 FAILED 重新切到 RUNNING 时会先重置失败阶段，让用户重新开始任务时
        不需要手动清理错误状态。
        """
        if not self.stages:
            self.status = status
            return self.status

        for stage in self.stages:
            if stage.status == TaskStatus.COMPLETED:
                continue
            if status == TaskStatus.RUNNING and stage.status == TaskStatus.FAILED:
                stage.reset(sync=False)
            stage.setStatus(status, sync=False)

        return self.updateStatus()

    def reset(self) -> TaskStatus:
        if not self.stages:
            self.status = TaskStatus.WAITING
            return self.status

        for stage in self.stages:
            stage.reset(sync=False)

        return self.updateStatus()

    def pendingStages(self) -> Iterable[TaskStage]:
        """按 stageIndex 顺序产出仍需执行的阶段。

        这是串行执行模型的边界：Task.run() 一次只运行一个 Stage。循环期间若
        任务状态被改为非 RUNNING，会停止继续产出，支持暂停和取消。
        """
        self.stages.sort(key=lambda stage: stage.stageIndex)
        for stage in self.stages:
            if self.status != TaskStatus.RUNNING:
                break
            if stage.status == TaskStatus.COMPLETED:
                continue
            yield stage

    def setSelection(self, selectedIndexes: list[int]):
        """同步多文件任务的文件选择和 Stage 列表。

        只有设置了 files 与 stageType 的任务支持选择。已有 Stage 会尽量保留，
        取消选择的文件对应 Stage 会被删除，新选文件再通过 stageType.fromFile()
        补齐，避免无谓丢失已下载进度。
        """
        if self.files is None or self.stageType is None:
            return

        selectedSet = set(selectedIndexes)

        for file in self.files:
            file.selected = file.index in selectedSet

        stagesToRemove = [
            stage for stage in self.stages
            if (fileIndex := getattr(stage, "fileIndex", None)) is not None
            and fileIndex not in selectedSet
        ]
        for stage in stagesToRemove:
            self.stages.remove(stage)

        existingFileIndexes = {
            fileIndex
            for stage in self.stages
            if (fileIndex := getattr(stage, "fileIndex", None)) is not None
        }
        for file in self.files:
            if file.selected and file.index not in existingFileIndexes:
                newStage = self.stageType.fromFile(file, self)
                self.addStage(newStage)

        self.fileSize = sum(f.size for f in self.files if f.selected)
        self.updateStatus()

    def applySettings(self, payload: dict):
        path = payload.get("path")
        if isinstance(path, (str, Path)):
            self.path = Path(path)

        if "category" in payload:
            self.category = payload["category"]

    def editorCards(self, parent):
        return []

    def tryKeepProgress(self, newTask: "Task") -> bool:
        # 子类默认不支持热替换，调用方会走 replaceWith。HttpTask 在文件大小和
        # Stage 数兼容时可只替换 URL/headers/proxies，从而保留已有进度。
        return False

    def replaceWith(self, newTask: "Task") -> None:
        # taskId、path、category 是用户本地身份和归档选择，替换任务时必须保留；
        # url、title、fileSize、stages 来自重新解析结果，需要整体覆盖。
        self.cleanup()
        self.url = newTask.url
        self.title = newTask.title
        self.fileSize = newTask.fileSize
        self.stages = newTask.stages
        for stage in self.stages:
            stage._bindTask(self)
        self.updateStatus()

    def cleanup(self):
        """清理任务输出和断点记录文件。

        Stage.cleanup() 先执行插件自定义清理；随后再删除通用 outputFolder、
        outputFile 和 Python HTTP 引擎的 .ghd 记录。Rust 引擎的 .ghdx 由对应
        Worker/引擎侧管理，避免这里误删非本任务格式的文件。
        """
        for stage in self.stages:
            stage.cleanup()

        targets: set[Path] = set()
        if self.outputFolder:
            targets.add(Path(self.outputFolder))
        for stage in self.stages:
            outputFile = getattr(stage, "outputFile", None)
            if outputFile:
                targets.add(Path(outputFile))

        for target in targets:
            removePath(target)
            removePath(Path(str(target) + ".ghd"))

    async def run(self):
        """串行执行所有待处理阶段。

        CoreService 负责并发任务数，Task 只保证本任务内部阶段顺序。Worker 抛出
        未处理异常时会写入当前 Stage.error 并继续向上抛，让调度层释放运行槽位。
        """
        currentStage = None
        try:
            for stage in self.pendingStages():
                currentStage = stage
                worker = stage.workerType(stage)
                await worker.run()
        except asyncio.CancelledError:
            logger.info("{} stopped", self.title)
            raise
        except Exception as e:
            if currentStage is not None and not currentStage.error:
                currentStage.setError(e)
            logger.opt(exception=e).error("{} failed", self.title)
            raise

    def serialize(self) -> bytes:
        """序列化任务到 JSON bytes，供 TaskService 写入 Memory.log。"""
        return dumps(_toSerializable(self))

    @classmethod
    def deserialize(cls, data: Any) -> "Task":
        """从持久化记录恢复任务对象。

        Task 和 TaskStage 都通过 type 字段查 registry。没有 type 或找不到子类时
        会退回基类，保证旧记录尽量可读，但插件特有字段可能因此被过滤掉。
        """
        if isinstance(data, (bytes, bytearray, str)):
            obj = loads(data)
        else:
            obj = data

        typeName = obj.pop("type", None)
        targetCls = Task._registry.get(typeName, cls) if isinstance(typeName, str) else cls

        if "status" in obj and isinstance(obj["status"], str):
            obj["status"] = TaskStatus[obj["status"]]
        if "path" in obj and isinstance(obj["path"], str):
            obj["path"] = Path(obj["path"])

        rawStages = obj.pop("stages", [])
        obj["stages"] = [TaskStage.deserialize(raw) for raw in rawStages]

        rawFiles = obj.pop("files", None)
        if rawFiles is not None and targetCls is cls:
            obj["files"] = [TaskFile(**_filterProperty(TaskFile, f)) for f in rawFiles]
        elif rawFiles is not None:
            obj["files"] = rawFiles

        return targetCls(**_filterProperty(targetCls, obj))

    def __hash__(self):
        return hash(self.taskId)


class PackConfig:
    """FeaturePack 配置基类。

    子类中声明的 ConfigItem 会被挂到 cfg.__class__ 上并立即 cfg.load()，
    这样插件配置能复用全局配置文件。新增配置项时要确保属性名稳定，否则用户
    已有配置键会变成孤儿字段。
    """

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)

        for attr_name, attr_value in cls.__dict__.items():
            if isinstance(attr_value, ConfigItem):
                setattr(cfg.__class__, f"pack_{cls.__name__}_{attr_name}", attr_value)

        cfg.load()

    def setupSettings(self, settingPage: "SettingPage"):
        raise NotImplementedError

    def dialogCards(self, parent: "QWidget") -> Iterable["SettingCard"]:
        return []

    def tr(self, text: str) -> str:
        return QCoreApplication.translate(self.__class__.__name__, text)
