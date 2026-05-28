from enum import Enum


class HttpEngine(Enum):
    """HTTP 下载实现选项，value 必须与配置文件和任务 metadata 中的字符串一致。"""

    PYTHON = "python"
    RUST = "rust"


def isRustEngineAvailable() -> bool:
    """检测 Rust 引擎是否可用。

    只捕获 ImportError：扩展存在但初始化失败时应让异常暴露到调用方或日志中，
    避免把 ABI/依赖问题误判为普通不可用。
    """
    try:
        import gd3_engine
        return True
    except ImportError:
        return False


def getRustEngineVersion() -> str | None:
    """获取 Rust 引擎版本号；未安装扩展时返回 None。"""
    try:
        import gd3_engine
        return gd3_engine.version()
    except ImportError:
        return None
