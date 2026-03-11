# VsCode IDE Instructions

## Critical Invariant

- 助记标识：`EOT=ASK_NEXT_TASK`。
- 最高优先级规则：只要代理判断当前任务已完成，就不得自主结束，必须先发起 AskQuestions。
- 每次 AskQuestions 都必须包含这个完全一致的固定选项：`必须按照文档执行 #file:copilot.instructions.md `。
- 若收口前发现本轮还没有 AskQuestions，或固定选项缺失，代理必须立即补发询问，禁止直接 final。

## Fixed Ask Template

- 默认问法模板：`下一步要我继续做什么任务？可多选，也可以直接输入新的具体任务。`
- 默认固定选项：`必须按照文档执行 #file:copilot.instructions.md `
- 当上下文被压缩、规则变弱或代理不确定时，应优先回忆助记标识 `EOT=ASK_NEXT_TASK`，并退回到本节模板执行。

## End-Of-Turn Rule

- 在每次准备结束当前回合、发送最终答复或标记“任务完成”时，必须先使用 AskQuestions 或等价平台接口向用户发起询问，禁止直接收口。
- 只要代理判断“当前用户任务已经完成”，就不得自主结束对话；必须立即进入“询问后续任务”流程。
- 若当前运行环境没有 AskQuestions 工具，必须改用仓库内兜底脚本：scripts/ask-questions-fallback.sh 或 scripts/ask-questions-fallback.ps1。
- 该询问在单次收口流程中只能调用一次；但只要进入下一轮并再次准备收口，就必须再次调用。
- 即使已经完成实现、验证、总结，只要还没有先询问用户下一步，就不得发送 final。
- 允许给出简短结果说明，但结果说明不能替代询问动作；询问后续任务是强制步骤。

## Trigger Conditions

出现下列任一情况时，必须先询问再收口：

- 准备发送 final 消息。
- 准备以“已完成”“先到这里”“任务结束”等语义结束当前回合。
- TODO、计划项或阶段目标被标记为完成。
- 已给出确定性结果，且代理判断自己可以结束当前工作。

## AskQuestions Requirements

- 问题必须简洁且紧贴当前上下文。
- 问题语义必须明确指向“下一步任务”或“后续要做什么”，不能只问“是否还有别的需要”这类弱约束问题。
- 必须允许多选。
- 必须允许自由输入。
- 必须至少提供 3 个预设选项。
- 预设选项里至少 1 个要直接指向新的可执行任务，而不是结束会话。
- 每次询问都必须包含一个固定选项：`必须按照文档执行 #file:copilot.instructions.md `，用于让用户在多轮对话、上下文压缩或行为漂移时显式要求代理重新严格遵守该指令入口。

## Fallback Script Requirements

- 优先级必须是：AskQuestions → 等价平台接口 → scripts/ask-questions-fallback.sh / scripts/ask-questions-fallback.ps1。
- 调用兜底脚本后，必须等待脚本返回结构化结果，再决定下一步；不得把“提示已打印”视为完成询问。
- 若 AskQuestions 或兜底脚本失败，必须按原任务上下文给出 3 个默认备选项，并重试最多 2 次。

## Turn Boundary

- 询问是收口前置动作，不是收口后的附带动作。
- 先完成工作，再询问；拿到用户选择后，再决定继续执行或等待用户，默认不得自主结束本轮。
- 不允许跳过询问直接结束。
- 询问的默认目标是“你接下来要我做什么”或“后续任务是什么”，而不是泛泛确认是否结束。
- 如果需要给出结果说明，说明应保持简短，并紧接着发起 AskQuestions；结果说明本身不构成结束动作。

## Pre-Final Self-Check

- 在任何 final 之前，代理必须先自检以下三项：
- 本轮是否已经发起 AskQuestions。
- AskQuestions 里是否包含固定选项 `必须按照文档执行 #file:copilot.instructions.md `。
- 当前问题是否明确指向下一步任务。
- 只要任一项不满足，就不得结束，必须先补齐后再继续。

## Session Logging

- 用户在询问中的选择必须记录到 /memories/session/，包含时间戳、任务标识、选择项与自由输入。
- 若用户选择 `必须按照文档执行 #file:copilot.instructions.md `，代理在后续动作前应先明确点名该文件，并复述其中的收口规则，再继续执行。
 