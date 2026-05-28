import asyncio
import sys
from pathlib import Path
from typing import Callable, Dict, Any, Coroutine

from PySide6.QtCore import QThread, QTimer, QStandardPaths, QResource, QFileInfo, Qt
from PySide6.QtWidgets import QApplication, QFileIconProvider
from desktop_notifier import DesktopNotifier, Icon, Button
from loguru import logger

from app.bases.models import Task, TaskStatus
from app.services.feature_service import featureService
from app.supports.config import cfg
from app.supports.utils import openFile

if sys.platform == 'win32':
    import winloop
    winloop.install()
elif sys.platform != 'darwin':
    import uvloop
    uvloop.install()

def getNotifierIcon() -> Path:
    """把 Qt 资源中的图标落盘，供桌面通知库按文件路径读取。"""
    _ = Path(QStandardPaths.writableLocation(QStandardPaths.StandardLocation.TempLocation) + "/gd3_logo.png")
    if not _.exists():
        with open(_, "wb") as f:
            f.write(QResource(":/image/logo.png").data())
    return _

class CoreService(QThread):
    """运行下载调度事件循环的后台线程。

    UI 线程只提交任务、暂停任务或投递协程；真正的 asyncio loop 固定在这个
    QThread 中运行。waitingTasks 是 FIFO 等待队列，runningTasks 保存 loop 中
    的 asyncio.Task，usesSlot=False 的任务不占用用户配置的并发下载槽。
    """

    def __init__(self):
        super().__init__()
        self.loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self.loop)
        self.mainLoop = self.loop.create_task(self.main())
        self.tasks: set[Task] = set()
        self.waitingTasks: list[Task] = []
        self.runningTasks: dict[str, asyncio.Task] = {}
        self._pendingCallbacks: Dict[str, Callable[[Any, str | None], Coroutine | None]] = {}
        cfg.maxTaskNum.valueChanged.connect(lambda _: self._rebalanceSoon())

    def sendNotification(self, task: Task):
        """发送任务完成通知。

        通知按钮会回到系统打开文件/目录。这里使用 outputFolder 的父目录，是为
        兼容单文件任务和目录型任务；缺失输出路径时只记录警告，不影响调度。
        """
        outputFolder = task.outputFolder
        if not outputFolder:
            logger.warning("task {} has no outputFolder for notification", task.taskId)
            return

        directoryPath = str(Path(outputFolder).parent)
        iconTempPath = Path(QStandardPaths.writableLocation(QStandardPaths.StandardLocation.TempLocation)) / "finished_file_icon.png"
        QFileIconProvider().icon(QFileInfo(outputFolder)).pixmap(48, 48).scaled(
            128,
            128,
            aspectMode=Qt.AspectRatioMode.KeepAspectRatio,
            mode=Qt.TransformationMode.SmoothTransformation,
        ).save(str(iconTempPath), "PNG")
        buttons = [
            Button(self.tr('打开文件'), lambda: openFile(outputFolder)),
            Button(self.tr('打开目录'), lambda: openFile(directoryPath)),
        ]
        self.loop.create_task(
            self.desktopNotifier.send(
                self.tr("下载完成"),
                task.title,
                buttons=buttons,
                on_clicked=lambda: openFile(outputFolder),
                icon=Icon(path=iconTempPath),
            )
        )


    def runCoroutine(self, coroutine: Coroutine, callback: Callable[[Any, str | None], Coroutine | None] | None = None):
        """在 CoreService 的事件循环中执行任意协程。

        callback 会被缓存到 _pendingCallbacks，并在协程结束后切回 Qt 主线程执行。
        返回空字符串表示没有注册回调，调用方无需取消。
        """
        if callback is not None:
            callbackId = f"custom_{id(callback)}_{hash(coroutine)}"

            self._pendingCallbacks[callbackId] = callback

            self.loop.create_task(self._runCoroutine(coroutine, callbackId))

            return callbackId

        return ""

    async def _runCoroutine(self, coroutine: Coroutine, callbackId):
        """执行外部提交的协程并统一捕获异常。"""
        try:
            result = await coroutine
            error = None
        except Exception as e:
            logger.opt(exception=e).error("异步任务执行失败 {}", callbackId)
            result = None
            error = repr(e)

        callback = self._pendingCallbacks.pop(callbackId, None)
        if callback is not None:
            self._executeCallback(callback, result, error)

    def _executeCallback(self, callback: Callable, result: Any, error: str = None):
        """线程安全地执行回调函数

        通过 Qt 的事件循环机制确保回调在主线程中执行，
        避免子线程直接操作 UI 导致的崩溃问题。

        Args:
            callback: 回调函数
            result: 成功结果
            error: 错误信息
        """

        def wrapper():
            try:
                if asyncio.iscoroutinefunction(callback):
                    self.loop.create_task(callback(result, error))
                else:
                    callback(result, error)
            except Exception as e:
                logger.opt(exception=e).error("回调函数执行失败")

        application = QApplication.instance()
        if application:
            QTimer.singleShot(0, application, wrapper)
        else:
            wrapper()

    async def _parse(self, payload: dict):
        """解析新增任务载荷；保留为 CoreService 内部异步入口。"""
        return await featureService.parse(payload)

    def _slotTaskIds(self) -> list[str]:
        """返回当前占用并发槽位的运行中任务 ID。"""
        taskIds: list[str] = []
        for taskId in self.runningTasks:
            task = self.task(taskId)
            if task is None or not task.usesSlot:
                continue
            taskIds.append(taskId)
        return taskIds

    def _removeWaitingTask(self, task: Task):
        self.waitingTasks = [queuedTask for queuedTask in self.waitingTasks if queuedTask.taskId != task.taskId]

    def _enqueueTask(self, task: Task):
        """把任务放回等待队列，并保证同一 taskId 不重复排队。"""
        self._removeWaitingTask(task)
        task.setStatus(TaskStatus.WAITING)
        self.waitingTasks.append(task)

    def _dispatchTask(self, task: Task):
        """把任务切为 RUNNING 并提交到后台事件循环。"""
        self._removeWaitingTask(task)
        task.setStatus(TaskStatus.RUNNING)
        self.runningTasks[task.taskId] = self.loop.create_task(self._runTask(task))

    def _rebalanceSoon(self):
        """从任意线程安全触发一次并发槽重平衡。"""
        if self.loop.is_running():
            self.loop.call_soon_threadsafe(lambda: self.loop.create_task(self._rebalance()))

    def rebalance(self):
        self._rebalanceSoon()

    def _scheduleWaitingTasks(self):
        """在并发槽有空位时按 FIFO 启动等待任务。"""
        while self.waitingTasks and len(self._slotTaskIds()) < cfg.maxTaskNum.value:
            task = self.waitingTasks.pop(0)
            if task.taskId in self.runningTasks:
                continue

            self._dispatchTask(task)

    async def _requeueTask(self, task: Task):
        """取消运行中的任务并重新放入等待队列。

        该路径用于用户降低最大并发数时收缩运行任务；已完成或失败的任务不会
        再入队，避免状态被意外改回 WAITING。
        """
        runningTask = self.runningTasks.get(task.taskId)
        if runningTask is None:
            self._enqueueTask(task)
            return

        if runningTask.cancel():
            try:
                await runningTask
            except asyncio.CancelledError:
                pass

        self.runningTasks.pop(task.taskId, None)

        if task.status not in {TaskStatus.COMPLETED, TaskStatus.FAILED}:
            self._enqueueTask(task)

    async def _rebalance(self):
        """按当前 cfg.maxTaskNum 收缩或补充运行任务。"""
        runningTaskIds = self._slotTaskIds()
        overflowTaskIds = runningTaskIds[cfg.maxTaskNum.value:]

        for taskId in overflowTaskIds:
            task = self.task(taskId)
            if task is None:
                continue
            await self._requeueTask(task)

        self._scheduleWaitingTasks()

    async def _runTask(self, task: Task):
        """执行单个任务并在结束后释放运行槽位。"""
        try:
            await task.run()
        finally:
            self.runningTasks.pop(task.taskId, None)
            self._scheduleWaitingTasks()

    def createTask(self, task: Task):
        """注册并启动任务。

        若并发槽已满，任务会进入 waitingTasks；否则立即 dispatch。重复 taskId
        正在运行时直接忽略，防止 UI 多次点击造成重复协程。
        """
        self.tasks.add(task)
        if task.taskId in self.runningTasks:
            return

        if task.usesSlot and len(self._slotTaskIds()) >= cfg.maxTaskNum.value:
            self._enqueueTask(task)
            return

        self._dispatchTask(task)

    async def _stopTask(self, task: Task):
        """从调度器中移除任务，并等待运行协程完成取消清理。"""
        self.tasks.discard(task)
        self._removeWaitingTask(task)
        runningTask = self.runningTasks.get(task.taskId)
        if runningTask is not None and runningTask.cancel():
            try:
                await runningTask
            except asyncio.CancelledError:
                pass
        self.runningTasks.pop(task.taskId, None)
        self._scheduleWaitingTasks()

    def stopTask(self, task: Task):
        """暂停任务并异步取消其后台执行。"""
        task.setStatus(TaskStatus.PAUSED)
        self.loop.create_task(self._stopTask(task))

    def task(self, taskId: str) -> Task | None:
        """根据任务Id获取任务对象

        Args:
            taskId: 任务Id

        Returns:
            Task: 对应的任务对象，如果不存在则返回None
        """
        for task in self.tasks:
            if task.taskId == taskId:
                return task
        return None

    def cancelCallback(self, callbackId: str) -> bool:
        """移除待执行的回调函数

        Args:
            callbackId: 回调函数标识符

        Returns:
            bool: 是否成功移除
        """
        if callbackId in self._pendingCallbacks:
            del self._pendingCallbacks[callbackId]
            return True
        return False

    async def main(self):
        """主事件循环

        在这里可以添加周期性任务，如清理过期回调、监控任务状态等
        """
        while True:
            try:
                await asyncio.sleep(1)

            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.opt(exception=e).error("CoreService 主循环发生错误")
                await asyncio.sleep(1)

    def run(self):
        """启动线程和事件循环"""
        self.desktopNotifier = DesktopNotifier(app_name="Ghost Downloader", app_icon=Icon(path=getNotifierIcon()))  # OSError: [WinError -2147417842] 应用程序调用一个已为另一线程整理的接口。
        try:
            self.loop.run_until_complete(self.mainLoop)
        except Exception as e:
            logger.opt(exception=e).error("CoreService 启动失败")
        finally:
            if self.loop:
                self.loop.close()

    def stop(self):
        """停止服务并清理内存状态。

        应用退出时调用；这里不负责持久化任务，TaskService.flushNow() 需要由
        更高层在退出流程中保证执行。
        """
        if self.loop and self.loop.is_running():
            if hasattr(self, 'mainLoop') and not self.mainLoop.done():
                self.mainLoop.cancel()

            self.loop.call_soon_threadsafe(self.loop.stop)

        self._pendingCallbacks.clear()
        self.tasks.clear()
        self.waitingTasks.clear()
        self.runningTasks.clear()
        cfg.maxTaskNum.valueChanged.disconnect()

coreService = CoreService()
