# Write budgets and branching time travel (P5a)

This document records the operational semantics implemented in both stores and
the exact command surface. The normative record layout and rules are in
`spec/WEFT-QUOTA.md`.

## Quotas

```bash
# install a policy at init (repeatable; or --quota-file FILE)
weftfs --store /s --quota 0:0:104857600:1000 init
weftfs --store /s --quota tenant:acme:1048576:10 create /a.txt --text hello

# caller's own usage only
weftfs --store /s --json quota show --uid 0 --gid 0
weftfs --store /s --json quota show --tenant acme

# explicit admin operations
weftfs --store /s quota reset      # zero counters, keep policy
weftfs --store /s quota rebuild    # recompute from the published commit chain
```

- Usage lives in the commit log as a `set_quota_usage` op appended to every
  commit while a policy is configured. It therefore survives reopen and cannot
  be left stale by a crash, with **no separate durable write per commit**. The
  store-root `quota` file is policy-only (`WFQ2`).
- Admission is checked before publication. A denial is
  `BudgetExceeded: write budget exceeded`, with no usage, limit, or principal
  disclosed. Generation, head, state, and usage are unchanged.
- `quota rebuild` replays the published chain, validates every committed usage
  assertion, and publishes a corrective op. `quota reset` publishes one reset
  op and zeroes the counters. `verify --full` re-derives usage and rejects an
  inconsistent assertion as `Corrupt`.
- A legacy `WFQ1` combined record is imported once on writable open: its exact
  bytes are preserved as `quota.migrated` and `quota` becomes `WFQ2`. If the
  two records exist and disagree (or a preserved record that names the current
  head disagrees with committed usage), writes fail closed with `Corrupt`.
- Tenant attribution uses the reserved object attribute `weft.tenant`, set by
  `create --tenant NAME` (both languages). The FUSE write path does not set it
  yet; API callers and fixtures can.

## Branches

```bash
weftfs --store /s snapshot create s5000
weftfs --store /s snapshot branch s5000 by:task=agent-43
weftfs --store /s context create --name base --selector 'select attrs.task == "agent-43"' --snapshot s5000
weftfs --store /s context branch base by:task=agent-43 --task agent-43 --model qwen-test
weftfs --store /s merge left-branch right-branch
```

`merge` is a **union** merge: identical content at the same path dedupes,
different content is `Conflict` naming the path, and the live head is never
overwritten. It is not a three-way content merge.
