import asyncio
import os
from asyncio import TaskGroup, CancelledError
from contextlib import suppress
from dataclasses import field, dataclass
from pathlib import Path
from struct import unpack, pack
from typing import ClassVar, TYPE_CHECKING

import niquests
from loguru import logger

from app.bases.interfaces import Worker
from app.bases.models import Task, TaskStage, TaskStatus, SpecialFileSize
from app.supports.config import cfg
from app.supports.sysio import ftruncate, pwrite
from app.supports.utils import splitCookies

if TYPE_CHECKING:
    from app.view.components.cards import ParseSettingCard


@dataclass(kw_only=True, eq=False)
class HttpTask(Task):
    """单文件 HTTP 下载任务。

    HTTP 任务通常只有一个 HttpTaskStage。编辑任务时若文件大小不变，可以只替换
    URL、请求头和代理配置，从而保留断点记录与已下载文件。
    """

    packId: str = "http"
    supportsEdit: ClassVar[bool] = True

    @property
    def stage(self) -> "HttpTaskStage":
        return self.stages[0]

    @property
    def headers(self) -> dict:
        return self.stage.headers

    @property
    def proxies(self) -> dict | None:
        return self.stage.proxies

    @property
    def blockNum(self) -> int:
        return self.stage.blockNum

    def editorCards(self, parent) -> list["ParseSettingCard"]:
        from app.view.components.add_task_dialog import SelectFolderCard
        from app.view.components.edit_task_cards import (
            HeadersEditCard,
            ProxiesEditCard,
            UrlEditCard,
        )
        from qfluentwidgets import FluentIcon

        return [
            UrlEditCard(FluentIcon.LINK, parent.tr("下载链接"), parent, initial=self.url),
            HeadersEditCard(FluentIcon.GLOBE, parent.tr("请求标头"), parent, initial=self.headers),
            ProxiesEditCard(FluentIcon.CERTIFICATE, parent.tr("代理服务器"), parent, initial=self.proxies),
            SelectFolderCard(FluentIcon.DOWNLOAD, parent.tr("下载到"), parent, initial=self.path),
        ]

    def applySettings(self, payload):
        """应用编辑对话框提交的 HTTP 参数。"""
        super().applySettings(payload)
        if "url" in payload:
            self.url = payload["url"]
            self.stage.url = payload["url"]
        if "headers" in payload:
            self.stage.headers = payload["headers"]
        if "proxies" in payload:
            self.stage.proxies = payload["proxies"]

    def tryKeepProgress(self, newTask: Task) -> bool:
        """在重新解析任务后尽量保留已有下载进度。

        只有新旧任务都是 HttpTask 且文件大小一致时才复用旧 Stage。Range 支持性
        会更新为新探测结果，避免服务器能力变化后继续使用错误下载策略。
        """
        if not isinstance(newTask, HttpTask):
            return False
        if self.fileSize <= 0 or self.fileSize != newTask.fileSize:
            return False
        oldStage, newStage = self.stage, newTask.stage
        oldStage.url = newStage.url
        oldStage.headers = newStage.headers
        oldStage.proxies = newStage.proxies
        oldStage.supportsRange = newStage.supportsRange
        self.url = newTask.url
        return True


@dataclass(kw_only=True)
class HttpTaskStage(TaskStage):
    """HTTP 下载阶段。

    engine 记录创建任务时选择的实现，反序列化后 __post_init__ 会重新判断 Rust
    引擎是否可用；不可用时自然回退到 Python Worker，保证历史任务可恢复。
    """

    workerType: type = field(init=False, repr=False)
    canPause: bool = field(init=False, default=True)

    url: str
    fileSize: int
    headers: dict
    proxies: dict
    blockNum: int
    supportsRange: bool = True
    accelerated: bool = False
    engine: str = "python"
    outputFileOverride: str = ""

    @property
    def outputFile(self) -> str:
        """阶段输出文件路径。

        默认写入任务目录下的任务标题文件；outputFileOverride 用于 FFmpeg/Bilibili
        等复用 HTTP Stage 时把资源写到特定临时路径。
        """
        return self.outputFileOverride or str(Path(self.task.path) / self.task.title)

    @outputFile.setter
    def outputFile(self, value: str):
        self.outputFileOverride = value

    def __post_init__(self):
        self.canPause = self.supportsRange
        # workerType 不参与序列化，因此任务恢复时必须重新根据 engine 选择实现。
        if self.engine == "rust":
            from app.supports.engine import isRustEngineAvailable
            if isRustEngineAvailable():
                from features.http_pack.rust_worker import RustHttpWorker
                self.workerType = RustHttpWorker
                return
        # 默认使用 Python Worker（由文件末尾的类属性赋值提供）


@dataclass
class HttpSubworker:
    """Python HTTP Worker 的一个字节范围下载单元。

    start 是分片起点，progress 是下一次写入偏移，end 是闭区间终点；特殊 end
    值来自 SpecialFileSize，用于表达未知大小或服务器不支持 Range。
    """

    start: int
    progress: int
    end: int


class HttpWorker(Worker):
    """基于 niquests 的 Python HTTP 下载实现。

    该 Worker 使用多个 HttpSubworker 并发写入同一文件，.ghd 文件保存每个分片
    的 start/progress/end 三元组，用于暂停后恢复。所有磁盘写入都走 pwrite，
    避免多个分片共享文件句柄时互相移动文件偏移。
    """

    def __init__(self, stage: HttpTaskStage):
        super().__init__(stage)
        self.stage = stage
        self.speedHistory = []
        self.accelCheckTime = 0
        self.requestHeaders, self.requestCookies = splitCookies(stage.headers)

    def reassignSubworker(self):
        """把剩余最多的分片拆成两半以提升慢连接场景的利用率。"""
        if self.stage.fileSize <= 0:
            return

        slowestSubworker = max(self.subworkers, key=lambda sw: sw.end - sw.progress + 1)
        remainingBytes = slowestSubworker.end - slowestSubworker.progress + 1
        if remainingBytes < cfg.maxReassignSize.value * 1048576:
            return
        base = remainingBytes // 2
        remainder = remainingBytes % 2
        oldEnd = slowestSubworker.end
        slowestSubworker.end = slowestSubworker.progress + base + remainder - 1
        newSubworker = HttpSubworker(slowestSubworker.end + 1, slowestSubworker.end + 1, oldEnd)
        self.subworkers.insert(self.subworkers.index(slowestSubworker) + 1, newSubworker)
        self.taskGroup.create_task(self.handleSubworker(newSubworker))

    def _buildRangeHeaders(self, rangeValue: str) -> dict:
        """构造 Range 请求头，并禁用压缩以保证字节偏移可对应原文件。"""
        requestHeaders = self.requestHeaders.copy()
        requestHeaders["range"] = rangeValue
        requestHeaders["accept-encoding"] = "identity"
        return requestHeaders

    async def handleSubworker(self, subworker: HttpSubworker):
        """下载单个分片，失败后按当前策略持续重试。

        分三种模式处理：未知大小但支持 Range、不支持 Range、普通固定范围。
        普通范围会截断超出分片边界的 chunk，防止服务器返回过量数据覆盖后续分片。
        """
        if subworker.end == SpecialFileSize.UNKNOWN:  # 支持断点续传, 但文件大小未知
            while True:
                try:
                    res = await self.client.get(
                        self.stage.url,
                        headers=self._buildRangeHeaders(f"bytes={subworker.progress}-"),
                        cookies=self.requestCookies,
                        proxies=self.stage.proxies,
                        verify=cfg.SSLVerify.value,
                        allow_redirects=True,
                        stream=True,
                    )
                    try:
                        res.raise_for_status()
                        if res.status_code != 206:
                            raise Exception(f"服务器拒绝了范围请求，状态码：{res.status_code}")

                        async for chunk in await res.iter_raw(chunk_size=65536):
                            if not chunk:
                                continue
                            await cfg.checkSpeedLimitation()
                            pwrite(self.fileHandle, chunk, subworker.progress)
                            chunkSize = len(chunk)
                            subworker.progress += chunkSize
                            cfg.globalSpeed += chunkSize
                    finally:
                        await res.close()

                    return
                except Exception as e:
                    logger.opt(exception=e).error(
                        "{} 的未知大小分片连接中断，5 秒后重试", self.stage.outputFile,
                    )
                    await asyncio.sleep(5)

        elif subworker.end == SpecialFileSize.NOT_SUPPORTED:  # 不支持断点续传
            while True:
                try:
                    ftruncate(self.fileHandle, 0)
                    subworker.progress = 0
                    requestHeaders = self.requestHeaders.copy()
                    requestHeaders.pop("range", None)

                    res = await self.client.get(
                        self.stage.url,
                        headers=requestHeaders,
                        cookies=self.requestCookies,
                        proxies=self.stage.proxies,
                        verify=cfg.SSLVerify.value,
                        allow_redirects=True,
                        stream=True,
                    )
                    try:
                        res.raise_for_status()
                        if res.status_code != 200:
                            raise Exception(f"服务器返回了异常状态码：{res.status_code}")

                        async for chunk in await res.iter_content(chunk_size=65536):
                            if not chunk:
                                continue
                            await cfg.checkSpeedLimitation()
                            pwrite(self.fileHandle, chunk, subworker.progress)
                            chunkSize = len(chunk)
                            subworker.progress += chunkSize
                            cfg.globalSpeed += chunkSize
                    finally:
                        await res.close()

                    ftruncate(self.fileHandle, subworker.progress)
                    return
                except Exception as e:
                    logger.opt(exception=e).error(
                        "{} 不支持断点续传，已从头开始重试", self.stage.outputFile,
                    )
                    await asyncio.sleep(5)

        else:  # 正常下载
            while subworker.progress <= subworker.end:
                try:
                    res = await self.client.get(
                        self.stage.url,
                        headers=self._buildRangeHeaders(f"bytes={subworker.progress}-{subworker.end}"),
                        cookies=self.requestCookies,
                        proxies=self.stage.proxies,
                        verify=cfg.SSLVerify.value,
                        allow_redirects=True,
                        stream=True,
                    )
                    try:
                        res.raise_for_status()
                        if res.status_code != 206:
                            raise Exception(f"服务器拒绝了范围请求，状态码：{res.status_code}")

                        async for chunk in await res.iter_raw(chunk_size=65536):
                            if not chunk:
                                continue
                            remainingBytes = subworker.end - subworker.progress + 1
                            if len(chunk) > remainingBytes:
                                chunk = chunk[:remainingBytes]
                            await cfg.checkSpeedLimitation()
                            pwrite(self.fileHandle, chunk, subworker.progress)
                            chunkSize = len(chunk)
                            subworker.progress += chunkSize
                            cfg.globalSpeed += chunkSize
                            if subworker.progress > subworker.end:
                                break
                    finally:
                        await res.close()

                    if subworker.progress > subworker.end:
                        subworker.progress = subworker.end + 1

                except Exception as e:
                    logger.opt(exception=e).error(
                        "{} 的分片连接中断，5 秒后重试", self.stage.outputFile,
                    )
                    await asyncio.sleep(5)

            self.reassignSubworker()

    def checkIfAutoAcceleration(self):
        """根据速度稳定性判断是否继续自动拆分分片。

        先观察 5 秒稳定速度，再临时增加分片数；若速度提升低于分片增长的 80%，
        说明服务器或链路不受益于更多连接，后续不再自动加速。
        """
        if self.stage.accelerated or not cfg.autoSpeedUp.value:
            return

        self.speedHistory.append(self.stage.speed)
        if len(self.speedHistory) > 5:
            self.speedHistory.pop(0)
        if len(self.speedHistory) < 5:
            return

        avgSpeed = sum(self.speedHistory) / len(self.speedHistory)
        if avgSpeed == 0:
            return

        maxDeviation = max(abs(speed - avgSpeed) / avgSpeed for speed in self.speedHistory)
        if maxDeviation > 0.15:
            return

        if self.accelCheckTime == 0:
            self.accelInitialWorkers = len(self.subworkers)
            self.accelInitialSpeed = avgSpeed
            self.accelCheckTime = asyncio.get_event_loop().time()

            for _ in range(4):
                self.reassignSubworker()
        else:
            elapsedTime = asyncio.get_event_loop().time() - self.accelCheckTime
            if elapsedTime <= 5:
                return

            currentWorkers = len(self.subworkers)
            workerIncreaseRatio = (currentWorkers - self.accelInitialWorkers) / self.accelInitialWorkers
            speedIncreaseRatio = (avgSpeed - self.accelInitialSpeed) / self.accelInitialSpeed

            if speedIncreaseRatio < 0.8 * workerIncreaseRatio:
                self.stage.accelerated = True
                logger.info(
                    "自动加速已禁用，subworker 增加比: {:.2%}, 速度提升比: {:.2%}",
                    workerIncreaseRatio, speedIncreaseRatio,
                )
            else:
                self.accelCheckTime = 0
                logger.info(
                    "继续自动加速，subworker 增加比: {:.2%}, 速度提升比: {:.2%}",
                    workerIncreaseRatio, speedIncreaseRatio,
                )

    async def supervisor(self):
        """周期性写入断点记录并刷新阶段进度。

        .ghd 只在 supportsRange=True 时生成；不支持 Range 的下载每次重试都从头
        开始，写断点文件没有意义也会误导恢复逻辑。
        """
        recordFileHandle = None
        if self.stage.supportsRange:
            recordFileHandle = open(Path(self.stage.outputFile + ".ghd"), "wb")
        try:
            self.stage.receivedBytes = sum(sw.progress - sw.start for sw in self.subworkers)
            while True:
                if recordFileHandle is not None:
                    data = tuple(val for sw in self.subworkers for val in (sw.start, sw.progress, sw.end))
                    recordFileHandle.seek(0)
                    recordFileHandle.write(pack("<" + "Q" * len(data), *data))
                    recordFileHandle.flush()
                    recordFileHandle.truncate()

                receivedBytes = sum(sw.progress - sw.start for sw in self.subworkers)
                self.stage.speed = receivedBytes - self.stage.receivedBytes
                self.stage.receivedBytes = receivedBytes
                if self.stage.fileSize > 0:
                    self.stage.progress = (receivedBytes / self.stage.fileSize) * 100
                else:
                    self.stage.progress = 0

                self.checkIfAutoAcceleration()
                await asyncio.sleep(1)
        except CancelledError:
            pass
        finally:
            if recordFileHandle is not None:
                recordFileHandle.close()

    def restoreProgress(self) -> bool:
        """从 .ghd 文件恢复分片进度。

        记录格式是连续的小端 uint64 三元组。解析失败会清空已读分片并返回 False，
        让调用方重新生成分片，避免使用半损坏记录覆盖输出文件。
        """
        recordFile = Path(self.stage.outputFile + ".ghd")
        if recordFile.exists():
            try:
                with open(recordFile, "rb") as f:
                    while True:
                        data = f.read(24)  # 每个 subworker 3 个 uint64, 共 24 字节
                        if not data:
                            break
                        start, progress, end = unpack("<QQQ", data)
                        self.subworkers.append(HttpSubworker(start, progress, end))
                return True
            except Exception as e:
                logger.opt(exception=e).error("恢复下载分片失败 {}", self.stage.outputFile)
                self.subworkers.clear()
                return False
        return False

    def generateSubworkers(self):
        """根据服务器能力和文件大小生成初始分片。"""
        if not self.stage.supportsRange:
            self.subworkers.append(HttpSubworker(0, 0, SpecialFileSize.NOT_SUPPORTED))
            return

        if self.stage.fileSize == SpecialFileSize.UNKNOWN:
            self.subworkers.append(HttpSubworker(0, 0, SpecialFileSize.UNKNOWN))
            return

        blockNum = min(self.stage.blockNum, self.stage.fileSize)
        step = self.stage.fileSize // blockNum
        start = 0
        for _ in range(blockNum - 1):
            end = start + step - 1
            self.subworkers.append(HttpSubworker(start, start, end))
            start = end + 1

        self.subworkers.append(HttpSubworker(start, start, self.stage.fileSize - 1))

    def _cleanupRecordFile(self):
        """删除 Python HTTP 引擎的断点记录文件。"""
        target = Path(self.stage.outputFile + ".ghd")
        try:
            if target.is_file() or target.is_symlink():
                target.unlink()
        except Exception as e:
            logger.opt(exception=e).error("failed to cleanup temporary file {}", target)

    async def run(self):
        """执行 HTTP 下载并维护阶段状态。

        成功完成时删除 .ghd；取消时保留 .ghd 并把阶段置为 PAUSED，以便下次
        restoreProgress() 续传。异常路径通过 setError() 交给任务层持久化。
        """
        self.taskGroup = TaskGroup()
        self.subworkers: list[HttpSubworker] = []
        self.client = niquests.AsyncSession(happy_eyeballs=True, pool_maxsize=256)
        self.client.trust_env = False
        shouldCleanupRecordFile = False
        Path(self.stage.outputFile).parent.mkdir(parents=True, exist_ok=True)

        restored = False
        if self.stage.supportsRange:
            restored = self.restoreProgress()
        else:
            self._cleanupRecordFile()

        if not restored:
            logger.info("正在为 {} 生成下载分片", self.stage.outputFile)
            self.generateSubworkers()
        else:
            logger.info("从进度文件恢复下载分片 {}", self.stage.outputFile)

        openMode = os.O_RDWR | os.O_CREAT
        if not self.stage.supportsRange:
            openMode |= os.O_TRUNC
        self.fileHandle = os.open(self.stage.outputFile, openMode, 0o666)

        if not restored and self.stage.fileSize > 0:
            try:
                ftruncate(self.fileHandle, self.stage.fileSize)
            except Exception as e:
                logger.opt(exception=e).error("{} 预分配文件大小失败", self.stage.outputFile)

        supervisor = asyncio.create_task(self.supervisor())

        try:
            async with self.taskGroup:
                for subworker in self.subworkers:
                    self.taskGroup.create_task(self.handleSubworker(subworker))

            self.stage.setStatus(TaskStatus.COMPLETED)
            shouldCleanupRecordFile = True
            logger.info("{} 下载完成", self.stage.outputFile)
        except CancelledError:
            self.stage.setStatus(TaskStatus.PAUSED)
            raise
        except Exception as e:
            self.stage.setError(e)
            raise
        finally:
            if not supervisor.done():
                supervisor.cancel()
                with suppress(asyncio.CancelledError):
                    await supervisor
            os.close(self.fileHandle)
            await self.client.close()
            if shouldCleanupRecordFile:
                self._cleanupRecordFile()


HttpTaskStage.workerType = HttpWorker
