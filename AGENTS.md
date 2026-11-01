# AGENTS.md

<!-- BEGIN loredex -->
## 仓库 Wiki(loredex)

本仓库在 `.wiki/` 下维护证据可回溯的 wiki(页面在 `.wiki/pages/*.md`;每条论断都带 `[[cite:path:Lx-Ly]]` 源码标记,并 pin 到 git 内容哈希)。

Agent 协议:

1. 先查 wiki 再读代码:`loredex search "<关键词>"` / `loredex show <页面>`(或 MCP `wiki_search` / `wiki_read`)。
2. 信任页面前先验证新鲜度:`loredex check`(或 MCP `wiki_check_freshness`);STALE 页面视为未验证,去读源码。
3. 修改代码后:`loredex auto` 自愈漂移引用,`loredex plan` 列出剩余写作任务。
4. 写页面时每条论断配一个行内 `[[cite:path:Lx-Ly]]`;无引用的正文会被拒绝。

请勿手改本区块;用 `loredex agents-md` 重新生成(`loredex agents-md --remove` 移除)。
<!-- END loredex -->
