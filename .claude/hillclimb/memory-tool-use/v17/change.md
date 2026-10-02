Steps 6 and 7 together, plus the merge of master; claude-sonnet-5-5. Same fixture and cases as v14/v15.
- Step 6: challenge wording (the challenge tool and its evidence field, MCP instructions, ENGRAM.md): challenge right away with the user's words, even when the code still matches the old memory.
- Step 7: scope-only rank lets a close scope match (exact file, its directory, a sibling) clear the threshold on its own criticality x decay. Before, the PreToolUse hook injected nothing for default-criticality memories.
- Hook rendering: every summary line first, then body previews in score order (shortened rather than dropped), the long preview to the best score; budget 1000 -> 1500; shorter truncation marker.
- Merge of master: the stdio handshake is answered before slow startup work (cold-start fix).
Attribution: revise (repo_disagrees) -> step 6; consulted on before_edit cases and hook delivery -> step 7.
