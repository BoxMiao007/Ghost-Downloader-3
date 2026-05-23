from enum import Enum


class HttpEngine(Enum):
    PYTHON = "python"
    RUST = "rust"


def isRustEngineAvailable() -> bool:
    """检测 Rust 引擎是否可用"""
    try:
        import gd3_engine
        return True
    except ImportError:
        return False


def getRustEngineVersion() -> str | None:
    """获取 Rust 引擎版本号，不可用时返回 None"""
    try:
        import gd3_engine
        return gd3_engine.version()
    except ImportError:
        return None
