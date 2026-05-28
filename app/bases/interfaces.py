from typing import TYPE_CHECKING

from app.bases.models import TaskStage

if TYPE_CHECKING:
    from app.view.windows.main_window import MainWindow
    from app.view.components.cards import TaskCard
    from app.view.components.cards import ResultCard
    from app.bases.models import Task, PackConfig


class Worker:
    """任务阶段的执行器基类。

    Worker 只持有一个 TaskStage，并由 CoreService 间接通过
    Task.run() 创建和调度。子类的 run() 必须是可取消的协程：
    收到 asyncio.CancelledError 时应尽快释放网络、文件句柄等资源，
    并把阶段状态同步为 PAUSED 或 FAILED，避免持久化时留下错误状态。
    """

    def __init__(self, stage: TaskStage):
        self.stage = stage

    async def run(self):
        """执行当前阶段。

        子类需要在成功时把 stage 标记为 COMPLETED，在可恢复取消时标记为
        PAUSED；未捕获异常会被 Task.run() 记录到当前阶段。
        """
        raise NotImplementedError


class FeaturePack:
    """下载能力插件的最小契约。

    FeatureService 通过 manifest.toml 动态加载 FeaturePack 子类，再按
    priority 和目录名排序匹配 URL。packId 必须和 Task.packId 保持一致，
    否则任务恢复后无法找到对应的卡片、编辑器和结果视图。
    """

    packId: str
    priority: int = 0
    config: "PackConfig | None" = None

    def matches(self, url: str) -> bool:
        """判断插件是否能处理 URL。

        入参已经由 FeatureService 补齐 scheme；实现应保持轻量且不能抛出
        可预期异常，因为匹配过程会按插件顺序频繁调用。
        """
        return False

    async def parse(self, payload: dict) -> "Task":
        """把用户输入或外部入口载荷解析为 Task。

        payload 至少包含 url，可包含 path、headers、proxies、filename 等
        插件自定义字段。实现应在这里完成必要探测，并返回可序列化的任务。
        """
        raise NotImplementedError

    def taskCard(self, task: "Task", parent=None) -> "TaskCard":
        """返回任务列表中的卡片组件；默认使用通用任务卡。"""
        from app.view.components.cards import UniversalTaskCard
        return UniversalTaskCard(task, parent)

    def resultCard(self, task: "Task", parent=None) -> "ResultCard":
        """返回完成页中的结果卡片组件；默认使用通用结果卡。"""
        from app.view.components.cards import UniversalResultCard
        return UniversalResultCard(task, parent)

    def setup(self, mainWindow: "MainWindow"):
        """插件加载后的初始化钩子。

        此时主窗口和设置页已经创建，插件可注册菜单、信号或后台服务。
        不要在这里执行耗时网络请求，避免阻塞启动流程。
        """
        pass
