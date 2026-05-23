import asyncio
from loguru import logger

from app.bases.interfaces import Worker
from app.supports.config import cfg


class RustHttpWorker(Worker):
    """Rust HTTP 引擎 Worker 薄包装"""

    async def run(self):
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
                    logger.info(f"Rust 引擎下载完成: {self.stage.outputFile}")
                    break
                elif progress.state == "failed":
                    raise RuntimeError(progress.error or "Rust 引擎下载失败")
                elif progress.state == "paused":
                    break

                await asyncio.sleep(0.5)
        except asyncio.CancelledError:
            handle.pause()
            raise
