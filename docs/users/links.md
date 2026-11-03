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
  proposes a `contradicts` link (with the evidence as its note), and automatic
  proposers will do the same.
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
