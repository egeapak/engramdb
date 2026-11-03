# Linking memories

A **link** (an *edge*) is a typed relation between two memories: this one
contradicts that one, supports it, was derived from it, replaces it. Links
are how a reviewer records what a pile of separate memories says *about each
other*.

```bash
engramdb link <src> <kind> <dst> [--note <text>] [--confidence <0-1>] [--status proposed|confirmed|rejected] [--store <spec>] [--global]
engramdb link <a> contradicts <b> --winner <a|b> [--note <text>]
engramdb unlink <src> <kind> <dst> [--store <spec>] [--global]
engramdb links <id> [--all] [--kind <kind>]... [--global]
engramdb challenge <id> --evidence <text> --against <other-id>
```

`engramdb get <id>` shows a memory's links under the memory itself. In
`--format json` (and in the MCP `get`) they are an `edges` object —
`{outbound, inbound, hidden}`, always present, empty when there are none —
beside the memory's own `links` list (its outbound links as stored).

## Kinds

| Kind | Direction | Meaning |
|---|---|---|
| `contradicts` | symmetric | The two cannot both be true. |
| `related_to` | symmetric | Loosely related. |
| `duplicates` | symmetric | The two say the same thing. |
| `supports` | `src → dst` | `src` is evidence for `dst`. |
| `derived_from` | `src → dst` | `src` was derived from `dst`. |
| `supersedes` | `src → dst` | `src` replaces `dst`. Same as `add --supersedes`: `dst`'s validity window closes. `unlink` reopens it, unless another memory also supersedes `dst` (that one then becomes its successor). A supersession has no review status — it is never proposed or rejected; to reject one, `unlink` it. |

A **symmetric** link is stored once, on whichever memory it was first written
to, and is listed on both. Linking the reverse direction updates that one copy.

Linking the same pair with the same kind again **updates** the link instead
of adding a second one, and changes only what you pass: `--status rejected`
alone keeps the note, confidence and origin. A **proposal** (`--status
proposed`, or `challenge --against`) never changes an existing link at all —
re-proposing cannot downgrade a confirmed link, revive a rejected one, or
replace a reviewer's note.

## Review status

Every link has a status:

- `confirmed` — the default for a link you write yourself.
- `proposed` — written by something that is not sure: `challenge --against`
  proposes a `contradicts` link (with the evidence as its note), and the
  [automatic proposers](#automatic-proposals) do the same.
- `rejected` — kept, not deleted, so the same link is not proposed again.
  Hidden from `links` and `get` unless you pass `--all`.

A `contradicts` link has a third resolution: `--winner <id>` says one side is
right. The link is rewritten as `<winner> supersedes <loser>` and the loser's
validity window closes (it stays queryable with `include_invalidated`).

```bash
engramdb challenge 019a3c --evidence "webpki rejects the gateway CA" --against 019a41
engramdb links 019a3c                      # the proposed contradiction
engramdb link 019a3c contradicts 019a41 --winner 019a41
```

## Where links live

Outbound links are stored in the memory's own `.md` file, in the hidden
`<!-- engramdb -->` block, so they are committed and travel with a clone. The
index keeps a derived `edges` table so that inbound links can be listed; it is
rebuilt by `engramdb reindex` like the rest of the index. A failed write to that
table never fails the memory write; `doctor` (Memory edges) reports a table out
of step with the files, and `reindex` rebuilds it.

A memory file that does not parse — an unresolved merge conflict in its hidden
block is the usual cause — keeps its index row, its vectors and its links
through every `reindex`. `reindex` and `doctor` name the file; fix it by hand
and reindex. Nothing in the file is reset or rewritten meanwhile.

**A link always records the target's full id**, never the prefix you typed:
a prefix that is unique today stops being unique as the store grows.

Proposed links can also be reviewed in the
[dashboard](./dashboard.md#reviewing-proposed-edges) (`engramdb dashboard
--allow-writes`), which offers the same three resolutions plus reject.

## Automatic proposals

When a memory is created (and when an MCP `update` re-embeds it), EngramDB
compares it with its nearest neighbours by embedding and may *propose* links.
Nothing it proposes is trusted until someone reviews it: a proposed link
changes no score and no retrieval result, even with the
[retrieval settings](#links-in-retrieval-opt-in) on.

- **Contradictions** — when `[nli].enabled` is on, the same NLI check that
  auto-challenges a contradiction also proposes a `contradicts` link to the
  other memory, `origin: nli`, with the NLI contradiction score as its
  confidence.
- **Similar memories** — a neighbour at or above
  `[links].related_threshold` (0.80) is proposed as `related_to`,
  `origin: similarity`, with the similarity as confidence. It becomes
  `duplicates` instead only when it is also at or above
  `[links].duplicate_threshold` (0.92) **and** NLI ran on the pair and found
  no contradiction: two memories can sit very close in embedding space and
  still disagree ("X is enabled" / "X is disabled"), and a proposed duplicate
  invites deleting one of them. Without `[nli].enabled`, close neighbours are
  proposed as `related_to`. Similarity proposals need only embeddings.

Proposing costs no extra model call: the neighbour search reuses the vectors
the memory was just embedded with (their mean direction), and the NLI check
is the one `[nli]` already runs. What it adds to `add` is the search and the
proposal writes. Measured on a debug build (`add`, 25 runs, median): about
+90 ms over `propose_similar = false` when nothing is proposed, and
+110–300 ms when proposals are written, depending on how many. Set
`propose_similar = false` if `add` latency matters more than suggestions.

The proposers follow the same rules as `link`, plus three of their own:

- **They never touch an existing link.** If *any* link already joins the two
  memories — confirmed, proposed, or rejected, of any kind — nothing is
  proposed. A link you rejected is therefore never proposed again.
- **They are capped.** A memory carries at most
  `[links].max_proposals_per_memory` (3) unreviewed automatic proposals,
  counted on both ends; contradictions take the slots first. Only proposals
  whose other end still exists and is still valid count, so a proposal to a
  deleted or superseded memory frees its slot. Two memories created at the
  same moment can each see the other's slot as free, so a memory can end up
  one or two proposals over the cap; nothing is lost, the cap is a soft
  limit.
- **They never run in a hook.** Only `add`/`create` (and MCP `update`) do
  this work, never the per-prompt or per-tool-call hooks.

A shared memory still never names a personal one: a proposal between them is
stored on the personal side. Each proposal records a `linked` usage event
from the `maintenance` surface. Switch them off with
`[links].propose_contradictions = false` / `propose_similar = false`; see
[configuration](./configuration.md).

## Links in retrieval (opt-in)

By default links change no score and no query result. Four settings, all
off, let confirmed links take part in retrieval. None of them reads a
*proposed* or *rejected* link, and none follows a link into another store
(whether that end is live cannot be checked from here, and an unknown end is
not evidence).

| Setting | Default | Effect |
|---|---|---|
| `[retrieval.scoring] contradicts_penalty` | `0.0` | Subtracted once from a memory with a confirmed `contradicts` link to a **live** memory (it exists and is neither invalidated nor expired). Both ends pay it: without a reviewer's verdict neither side is known to be right. |
| `[retrieval.scoring] supports_boost` | `0.0` (max `0.1`) | Multiplier `1 + boost × min(n, 3)` for `n` live memories with a confirmed `supports` link **into** this one. Supporting something is not evidence for yourself, so outgoing links do not count. |
| `[retrieval.scoring] usage_weight` | `0.0` (max `0.2`) | Usage boost; see [configuration](./configuration.md#links-and-usage-in-scoring). Not a link setting, listed here because it rides the same path. |
| `[retrieval] expand_links` | `0` (max `20`) | Default for `query --expand-links`: add up to N one-hop neighbours of the top results. |

**Turning `contradicts_penalty` on can hide both sides of a contradiction
from the hooks.** The penalty also counts against the relevance test a
scope-only match uses to clear the rank threshold, so a file-scoped memory
whose own relevance (criticality × freshness) is less than the threshold
plus the penalty no longer clears it — and both ends of the contradiction
pay. The SessionStart and PreToolUse hooks return no weak fallback matches,
so they then inject *neither* memory, on exactly the file the dispute is
about. This is the knob doing what it says (a disputed memory ranks lower
everywhere), not an exemption the hooks get; if you would rather see the
conflict, resolve it (`--winner`, or reject the link) instead of raising the
penalty, or keep the penalty below the gap between your memories'
criticality and `[retrieval] relevance_threshold`.

A contradiction you resolve with `--winner` is no longer a `contradicts`
link: it becomes `<winner> supersedes <loser>`, the loser is invalidated and
so hidden from queries by default, and only the loser is affected. A
contradiction whose other side was superseded, expired or deleted stops
penalizing at once.

### Following links from results

```bash
engramdb query "deploy freeze" --mode filter --expand-links=3
engramdb query "deploy freeze" --mode filter --expand-links      # bare flag: 5
engramdb query --mode filter --expand-links "deploy freeze"      # still the query, N = 5
```

The value needs the `=` (`--expand-links=3`): a word after the bare flag is
always your query, never N.

MCP: `query` with `"expand_links": 3`. From each direct result (not from a
weak fallback match), confirmed `supports`, `derived_from` and `related_to`
links are followed one hop in either direction. `contradicts` is never
followed (its effect is the penalty), nor `duplicates` (the same content
again) or `supersedes` (one end is invalidated).

An added result is marked with the hit and link it came through (`via
<id> (supports)` in text output, `"via": {"from", "kind"}` in JSON) and is
scored at the hit's score × `[retrieval] expand_links_decay` (0.5) × its own
situation weight, **minus its own challenge and `contradicts` penalties** —
a disputed memory does not ride in at full strength on a healthy hit — or at
its own score when the query scored it and that is higher. Its score
breakdown is its own, so a contradicted neighbour still shows
`contradicts … from <id>`. It never ranks above the hit it came from. Up to
N are added **per query** (not per store, when groups or the global store
fan in), on top of `--max-results`. When results from stores embedded with
different models are re-ranked together (cross-store equalization), only
the direct results are re-ranked and cut to `--max-results`; the neighbours
of the survivors are then put back, each capped at its hit's new score.

A result reached only through a link is not counted as *returned* in usage
statistics: it did not survive ranking, so it cannot re-confirm (and
promote) a task-bound memory.

**Expansion never shows you a memory the query would not have been allowed to
return.** A neighbour must pass every filter the query applies — type, tags,
epistemic class, minimum criticality, expiry, validity (invalidated memories
stay hidden unless `--include-invalidated`), and in filter mode the path and
logical scope filters. For group and global memories the `audience` filter
applies to neighbours like to any result, and a neighbour reached only through
a memory you cannot see is dropped too. Hooks never expand.

## Rules worth knowing

- **Personal memories stay out of git.** A shared memory's file is committed,
  so it must not name a personal memory — in any direction. A symmetric link
  between a shared and a personal memory is stored on the personal side
  instead (the output says so); a directed link from a shared memory to a
  personal one is refused; so is a supersession between a shared and a
  personal memory either way (each file names the other), including through
  `--winner` and `add --supersedes`. Changing a memory's visibility is refused
  when it would break the rule: personal → shared while its links name a
  personal memory, shared → personal while a shared file names it. Group and
  global stores are not in a repository, so the rule does not apply there.
- **Other stores.** `--store global`, `--store group:<name>` or `--store
  <project-id>` points a link at a memory in another store. Give the full id;
  if that store cannot be opened on this machine the link is still recorded
  and shown as `store not reachable` — never as dangling.
- **Deleting a memory leaves links into it.** They are *dangling*: hidden by
  default, listed by `links --all`, and reported by `engramdb doctor` (Memory
  edges, a warning). Nothing prunes them automatically — a memory deleted on
  one branch can come back with a checkout.
- **Invalidated memories keep their links**, hidden by default. A link into a
  superseded memory names its successor.
- **Links are not challenges.** `resolve --keep` clears a memory's challenges
  and leaves its links alone. `gc` never edits links.
- **`compress`** moves the sources' outbound links onto the summary (as
  `origin: compress`). Links *into* a source keep pointing at it; since the
  source is now superseded, they show the summary as its successor.
