from pathlib import Path

import orjson
from PySide6.QtCore import QObject, QTimer, Signal, Slot
from loguru import logger

from app.bases.models import Task
from app.supports.paths import APP_DATA_DIR


class TaskService(QObject):
    """任务内存表与 Memory.log 持久化服务。

    CoreService 负责运行任务，TaskService 负责保存和恢复任务定义。写文件通过
    200ms debounce 合并短时间内的多次变更，并采用临时文件替换，降低崩溃时
    写出半截 JSONL 的概率。
    """

    taskAdded = Signal(object)
    taskRemoved = Signal(str)

    # Queued internal trigger: cross-thread emit hops to GUI event loop,
    # so scheduleFlush() is safe from any thread without a mutex.
    _flushRequested = Signal()

    def __init__(self):
        super().__init__()
        self.recordFile = Path(APP_DATA_DIR) / "Memory.log"
        if not self.recordFile.exists():
            self.recordFile.parent.mkdir(parents=True, exist_ok=True)
            self.recordFile.touch()

        self.tasks: dict[str, Task] = {}
        self._loaded = False

        self._flushTimer = QTimer(self)
        self._flushTimer.setSingleShot(True)
        self._flushTimer.setInterval(200)
        self._flushTimer.timeout.connect(self._flush)

        self._flushRequested.connect(self._onFlushRequested)

    def load(self):
        """从 Memory.log 读取全部任务。

        必须在允许 flush 前调用；_flush 会检查 _loaded，防止启动期间空内存表
        意外覆盖已有任务记录。
        """
        self.tasks = self._readAll()
        self._loaded = True

    def _readAll(self) -> dict[str, Task]:
        """读取 JSONL 任务记录。

        单行损坏只跳过该任务并记录错误，不能阻止其他任务恢复；这是用户数据文件
        的容错边界。
        """
        tasks: dict[str, Task] = {}
        with open(self.recordFile, "r", encoding="utf-8") as f:
            lines = f.readlines()

        for line in lines:
            line = line.strip()
            if not line:
                continue
            try:
                obj = orjson.loads(line)
                task = Task.deserialize(obj)
                tasks[task.taskId] = task
            except Exception as e:
                logger.opt(exception=e).error("failed to parse task record")
        return tasks

    def add(self, task: Task):
        """新增任务并安排持久化。"""
        if task.taskId in self.tasks:
            raise ValueError(f"task {task.taskId} already exists")
        self.tasks[task.taskId] = task
        self.scheduleFlush()
        self.taskAdded.emit(task)

    def remove(self, task: Task):
        """删除任务记录并通知 UI。"""
        if task.taskId not in self.tasks:
            return
        taskId = task.taskId
        del self.tasks[taskId]
        self.scheduleFlush()
        self.taskRemoved.emit(taskId)

    def scheduleFlush(self):
        """Coalesce bursts via 200ms debounce. Safe from any thread."""
        self._flushRequested.emit()

    def flushNow(self):
        """Force synchronous flush. Use only at shutdown."""
        if self._flushTimer.isActive():
            self._flushTimer.stop()
        self._flush()

    @Slot()
    def _onFlushRequested(self):
        """在 QObject 所在线程启动 debounce 定时器。"""
        self._flushTimer.start()

    @Slot()
    def _flush(self):
        """把当前任务表完整写回 Memory.log。

        这里采用全量重写而不是追加日志，是为了让删除任务和任务字段变化能直接
        反映到磁盘；写入失败时保留旧 recordFile，下一次 flush 可继续尝试。
        """
        if not self._loaded:
            logger.warning("skip flush because task service has not been loaded")
            return

        lines: list[str] = []
        for task in self.tasks.values():
            try:
                lines.append(task.serialize().decode("utf-8") + "\n")
            except Exception as e:
                logger.opt(exception=e).error("failed to serialize task {}", task.taskId)

        tempFile = self.recordFile.with_name(self.recordFile.name + ".tmp")
        try:
            with open(tempFile, "w", encoding="utf-8") as f:
                f.writelines(lines)
            tempFile.replace(self.recordFile)
        except Exception as e:
            logger.opt(exception=e).error("failed to write task record file")


taskService = TaskService()
