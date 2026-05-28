import asyncio
from loguru import logger

from app.bases.interfaces import Worker
from app.bases.models import TaskStatus
from app.supports.config import cfg


class RustHttpWorker(Worker):
    """Rust HTTP 引擎 Worker 薄包装。

    Python 侧只负责把 HttpTaskStage 转换成 gd3_engine.DownloadConfig，并轮询
    Rust 句柄的共享进度。真实下载、断点记录和限速逻辑在 Rust 扩展内完成。
    """

    async def run(self):
        """启动 Rust 下载并把引擎状态同步回 Stage。

        Rust 引擎使用 .ghdx 作为断点记录，和 Python Worker 的 .ghd 格式不同。
        取消任务时调用 handle.pause()，让扩展有机会落盘进度后再把阶段置为 PAUSED。
        """
        import gd3_engine

        config = gd3_engine.DownloadConfig(
            url=self.stage.url,
            output_path=self.stage.outputFile,
            headers=self.stage.headers or {},
            proxies=self.stage.proxies or {},
            file_size=self.stage.fileSize if self.stage.fileSize > 0 else -1,
            supports_range=self.stage.supportsRange,
            speed_limit=cfg.speedLimitation.value if cfg.enableSpeedLimitation.value else 0,
            max_connections=self.stage.blockNum,
            verify_ssl=cfg.SSLVerify.value,
            resume_file=self.stage.outputFile + ".ghdx" if self.stage.supportsRange else None,
        )

        handle = gd3_engine.start_download(config)
        self.stage._rustHandle = handle

        logger.info(f"Rust 引擎启动下载: {self.stage.url}")

        try:
            while True:
                progress = handle.progress
                self.stage.receivedBytes = progress.received_bytes
                self.stage.speed = progress.speed

                if progress.total_bytes > 0:
                    self.stage.progress = progress.percent

                if progress.state == "completed":
                    self.stage.progress = 100.0
                    self.stage.setStatus(TaskStatus.COMPLETED)
                    logger.info(f"Rust 引擎下载完成: {self.stage.outputFile}")
                    break
                elif progress.state == "failed":
                    raise RuntimeError(progress.error or "Rust 引擎下载失败")
                elif progress.state == "paused":
                    self.stage.setStatus(TaskStatus.PAUSED)
                    break

                await asyncio.sleep(0.5)
        except asyncio.CancelledError:
            handle.pause()
            self.stage.setStatus(TaskStatus.PAUSED)
            raise
