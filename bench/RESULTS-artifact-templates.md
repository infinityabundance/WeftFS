# T077 — evaluation and synthetic-data artifact templates

Receipt: `bench/receipts/artifact-templates-20261115T124915Z.json` (stamp
`20261115T124915Z`, WeftFS revision `d6e5957`). Raw log beside it. Normative
record definition:
[`spec/WEFT-ARTIFACT-TEMPLATES.md`](../spec/WEFT-ARTIFACT-TEMPLATES.md).

## 1. What was built

Two canonical templates, both content-addressed WCB records:

* `weft.eval-run.v1` — `config`, `dataset` and `model` roots, metric values as
  exact IEEE-754 binary64 bit patterns, and the declared outputs;
* `weft.synth-data-run.v1` — `config` and `generator` roots, the seed bytes
  verbatim, and the produced dataset shards.

Recording a run publishes the record, a matching `weft.provenance.v1` receipt
with the same edges (so the taint tracer walks through an evaluation or a
generation run), and an `artifact_template` relation on every output object so a
reader holding an artifact can find the declaration.

`replay` re-reads every declared input **by content root** and every recorded
output through the store's verified read path. It does not re-run the
evaluation, the training step or the generator.

## 2. What replay establishes, and what it does not

Establishes: the declared inputs are present and intact; the recorded outputs
are present and intact; the record is canonical and content-addressed, so the
declaration itself has not been altered; and the graph edges exist, so the
taint tracer reaches the same closure.

Does **not** establish: numerical or behavioural determinism; bitwise
reproducibility of any kernel; floating-point associativity; that the recorded
metrics followed from the recorded inputs; or that the declared environment is
the environment that actually ran. Reproducibility is an environment property,
and no claim about it is made here.

## 3. Method

Harness `examples/artifact_template_court.rs`, court
`scripts/artifact_template_court.sh`, Rust court. One evaluation run over a
4 MiB metrics artifact and one synthetic-data run with a 4-byte seed, recorded
through the shipped API and replayed seven times each. Template bytes are the
canonical WCB record.

## 4. Results

<!-- BEGIN GENERATED:artifact-template-costs (scripts/perf_numbers.py) -->
The evaluation record is **496 B** and the synthetic-data record is **375 B**. Recording the evaluation run takes **38,674,270 ns**; replaying it, which re-reads its **4,194,304 B** metrics artifact plus the three declared inputs, takes **1,252,089 ns**; replaying the synthetic-data run takes **18,374 ns**.
<!-- END GENERATED:artifact-template-costs -->

A template costs a canonical WCB record and a replay pass over the declared
bytes; both are dominated by the artifact sizes involved, not by the record. The
recording time above is the *first* commit after the 4 MiB metrics artifact was
created, so it carries that commit's durable barrier; it is a fixture artefact,
not the incremental cost of the record.

## 5. The negative case

The gate asks for a template whose declared input root does not match the store.
`record_eval_run` and `record_synth_run` resolve every declared input against
live state before writing anything:

* an absent root is `NotFound` with a message naming the role, and the harness
  checks that the template count did not change;
* the zero hash is `Malformed`;
* an absent output object is `NotFound`;
* a refused record publishes no blob — the suite compares the manifest directory
  before and after.

## 6. Losses

1. **A missing output makes replay inexact, not failed.** `exact=false` and the
   output entry lists no objects. A caller must look at `exact`.
2. **The seed is recorded verbatim.** That is a deliberate disclosure without
   which a later replay attempt is impossible; a caller that will not disclose a
   seed must not record a synthetic-data template.
3. **Several live objects may share a declared input root.** Replay reports the
   count; a template does not pick one and does not treat a copy as ambiguity.
4. **The environment block is a declaration.** It is copied into the provenance
   receipt and the template; nothing verifies it.
5. **A template is not a schema for arbitrary runs.** Only the two structures
   above are defined; an RL trajectory or a distillation run would need its own
   versioned schema.
6. **No Go parity.** WeftFS-Go rejects both schemas.
7. One host, one run, one fixture.

## 7. Non-claims

* No numerical or behavioural determinism claim, for either template.
* No claim that replaying a template reproduces an experiment.
* No fleet, cloud or GPU claim.
