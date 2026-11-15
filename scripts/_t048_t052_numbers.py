#!/usr/bin/env python3
"""One-shot helper used while authoring T048-T052: append manifest entries.

Run once from the repository root. It is idempotent by key: an existing key is
never duplicated or rewritten, so re-running after a partial failure is safe.
"""

import json
from pathlib import Path

MANIFEST = Path("docs/perf-numbers.json")

R48 = "bench/receipts/adapters-20261115T052218Z.json"
S48 = "20261115T052218Z"
V48 = "3a02e561a4d193e67554be882e52244d450d39c9"

R49 = "bench/receipts/training-state-20261115T054734Z.json"
S49 = "20261115T054734Z"
V49 = "3a02e561a4d193e67554be882e52244d450d39c9"

R51 = "bench/receipts/cross-model-delta-20261115T064249Z.json"
S51 = "20261115T064249Z"
V51 = "cad2f730bd16d0b8695fce89cc0132cff90f0188"

R52 = "bench/receipts/view-identity-20261115T070334Z.json"
S52 = "20261115T070334Z"
V52 = "cad2f730bd16d0b8695fce89cc0132cff90f0188"

GROUPS = [
    {"id": "t048", "title": "T048 typed adapters, merged views and the family cost"},
    {"id": "t049", "title": "T049 optimizer state, gradients and attention masks"},
    {"id": "t051", "title": "T051 cross-model lossless structural delta"},
    {"id": "t052", "title": "T052 exact versus approximate view identity"},
]

NUMBERS = []
BLOCKS = []


def n(key, label, unit, group, fmt, receipt=None, path=None, stamp=None, rev=None, expr=None,
      lst=None, where=None, select=None):
    entry = {"key": key, "label": label, "unit": unit, "group": group, "format": fmt}
    if expr is not None:
        entry["expr"] = expr
    else:
        entry["receipt"] = receipt
        if lst is not None:
            entry["list"] = lst
            if where:
                entry["where"] = where
            entry["select_path"] = select
        else:
            entry["path"] = path
        entry["stamp_path"] = "stamp"
        entry["stamp"] = stamp
        entry["revision_path"] = "revision.weftfs"
        entry["revision"] = rev
    NUMBERS.append(entry)


def block(bid, file, rows, inline=False):
    spec = {"id": bid, "file": file, "kind": "lines", "rows": [{"template": row} for row in rows]}
    if inline:
        spec["inline"] = True
    BLOCKS.append(spec)


# ---------------------------------------------------------------- T048
n("t048.adapters", "Adapters ingested", "", "t048", "int", R48, "corpus.adapter_count", S48, V48)
n("t048.merges", "Adapters merged into views", "", "t048", "int", R48, "corpus.merge_eligible", S48, V48)
n("t048.adapter_logical_b", "Adapter logical payload", "B", "t048", "int", R48,
  "families.adapter_logical_total", S48, V48)
n("t048.adapter_physical_b", "Adapters stored (all metadata)", "B", "t048", "int", R48,
  "families.adapter_physical_total", S48, V48)
n("t048.adapter_zstd_b", "`zstd -19` of the adapter files", "B", "t048", "int", R48,
  "families.adapter_zstd19_total", S48, V48)
n("t048.base_physical_b", "Base checkpoint stored", "B", "t048", "int", R48, "base.physical_bytes", S48, V48)
n("t048.base_zstd_b", "`zstd -19` of the base container", "B", "t048", "int", R48,
  "base.zstd19_container_bytes", S48, V48)
n("t048.family_physical_b", "Base plus all adapters stored", "B", "t048", "int", R48,
  "families.base_plus_all_adapters_physical", S48, V48)
n("t048.native_bytes", "Base plus adapter files as plain bytes", "B", "t048", "int", R48,
  "native.bytes_read", S48, V48)
n("t048.views_family_b", "Base plus every merged view stored", "B", "t048", "int", R48,
  "families.base_plus_all_merged_views_physical", S48, V48)
n("t048.views_logical_b", "Merged-view logical bytes", "B", "t048", "int", R48,
  "families.merged_logical_total", S48, V48)
n("t048.full_standalone_b", "One full merged model in a fresh store", "B", "t048", "int", R48,
  "merged_full_checkpoint.standalone_store_physical_bytes", S48, V48)
n("t048.full_shared_b", "Same full merged model beside the view", "B", "t048", "int", R48,
  "merged_full_checkpoint.shared_store_incremental_bytes", S48, V48)
n("t048.base_write_ms", "Base checkpoint write", "ms", "t048", "fixed:1", R48, "base.write_ms", S48, V48)
n("t048.base_restore_ms", "Base restore (whole container)", "ms", "t048", "fixed:1", R48,
  "base.restore_ms", S48, V48)
n("t048.view_merge_ms", "Merged view: merge", "ms", "t048", "fixed:0", R48,
  "merged_views.0.merge_ms", S48, V48)
n("t048.view_apply_ms", "Apply the adapter to a restored base", "ms", "t048", "fixed:0", R48,
  "merged_views.0.apply_ms", S48, V48)
n("t048.view_restore_ms", "Merged view: restore", "ms", "t048", "fixed:0", R48,
  "merged_views.0.restore_ms", S48, V48)
n("t048.view_targets", "Targets in the first merged view", "", "t048", "int", R48,
  "merged_views.0.merged_tensors", S48, V48)
n("t048.aneesa_restore_ms", "A 48-target merged view: restore", "ms", "t048", "fixed:1", R48,
  "merged_views.1.restore_ms", S48, V48)
n("t048.full_write_ms", "Full merged model: write (fresh store)", "ms", "t048", "fixed:1", R48,
  "merged_full_checkpoint.standalone_store_write_ms", S48, V48)
n("t048.full_restore_ms", "Full merged model: restore", "ms", "t048", "fixed:1", R48,
  "merged_full_checkpoint.standalone_store_restore_ms", S48, V48)
n("t048.small_physical_b", "Smallest merged adapter stored", "B", "t048", "int", R48,
  "adapters", S48, V48, lst="adapters",
  where={"repo": "Hazde/careerbot_PG6_Qwen_Qwen2.5-0.5B-Instruct_model_LoRA"},
  select="stored_physical_bytes")
n("t048.small_zstd_b", "Smallest merged adapter under `zstd -19`", "B", "t048", "int", R48,
  lst="adapters", where={"repo": "Hazde/careerbot_PG6_Qwen_Qwen2.5-0.5B-Instruct_model_LoRA"},
  select="zstd19_file_bytes", stamp=S48, rev=V48)
n("t048.small_ratio", "Smallest adapter, stored / `zstd -19`", "x", "t048", "ratio:3", R48,
  lst="adapters", where={"repo": "Hazde/careerbot_PG6_Qwen_Qwen2.5-0.5B-Instruct_model_LoRA"},
  select="physical_vs_zstd19", stamp=S48, rev=V48)
n("t048.small_source_b", "Smallest adapter source file", "B", "t048", "int", R48,
  lst="adapters", where={"repo": "Hazde/careerbot_PG6_Qwen_Qwen2.5-0.5B-Instruct_model_LoRA"},
  select="source_file_bytes", stamp=S48, rev=V48)
n("t048.big_physical_b", "Largest adapter stored", "B", "t048", "int", R48,
  lst="adapters", where={"repo": "SykoSLM/qwen2.5-0.5b-instruct-turkish-finetuned-lora"},
  select="stored_physical_bytes", stamp=S48, rev=V48)
n("t048.big_zstd_b", "Largest adapter under `zstd -19`", "B", "t048", "int", R48,
  lst="adapters", where={"repo": "SykoSLM/qwen2.5-0.5b-instruct-turkish-finetuned-lora"},
  select="zstd19_file_bytes", stamp=S48, rev=V48)
n("t048.big_ratio", "Largest adapter, stored / `zstd -19`", "x", "t048", "ratio:3", R48,
  lst="adapters", where={"repo": "SykoSLM/qwen2.5-0.5b-instruct-turkish-finetuned-lora"},
  select="physical_vs_zstd19", stamp=S48, rev=V48)
n("t048.big_source_b", "Largest adapter source file", "B", "t048", "int", R48,
  lst="adapters", where={"repo": "SykoSLM/qwen2.5-0.5b-instruct-turkish-finetuned-lora"},
  select="source_file_bytes", stamp=S48, rev=V48)
n("t048.ref_elements", "Reference tensor elements", "", "t048", "int", R48,
  "reference_merge.tensors.0.elements", S48, V48)
n("t048.ref_mismatch_f32", "NumPy float32 matmul vs Rust after bf16 rounding", "", "t048", "int", R48,
  "reference_merge.tensors.0.f32_matmul_mismatches", S48, V48)
n("t048.ref_mismatch_f64", "NumPy float64 accumulation vs Rust after bf16 rounding", "", "t048", "int", R48,
  "reference_merge.tensors.0.f64_accumulation_mismatches", S48, V48)
n("t048.ref_max_abs", "Largest reference difference", "", "t048", "fixed:6", R48,
  "reference_merge.tensors.0.max_abs_difference_vs_f32_matmul", S48, V48)
n("t048.adapter_vs_zstd_pct", "Adapters stored below `zstd -19`", "%", "t048", "pct:2",
  expr="(1 - {t048.adapter_physical_b} / {t048.adapter_zstd_b}) * 100")
n("t048.base_vs_zstd_pct", "Base stored below `zstd -19`", "%", "t048", "pct:2",
  expr="(1 - {t048.base_physical_b} / {t048.base_zstd_b}) * 100")
n("t048.family_vs_native_pct", "Family stored below the plain files", "%", "t048", "pct:2",
  expr="(1 - {t048.family_physical_b} / {t048.native_bytes}) * 100")
n("t048.views_incremental_b", "Merged views incremental bytes", "B", "t048", "int",
  expr="{t048.views_family_b} - {t048.base_physical_b}")
n("t048.views_vs_base_plus_full", "Merged-view family / (base + one full merged model)", "x", "t048", "ratio:3",
  expr="{t048.views_family_b} / ({t048.base_physical_b} + {t048.full_standalone_b})")
n("t048.full_shared_ratio", "Standalone / shared-store full-model increment", "x", "t048", "ratio:0",
  expr="{t048.full_standalone_b} / {t048.full_shared_b}")
n("t048.view_vs_apply", "View restore / (base restore + apply)", "x", "t048", "ratio:3",
  expr="{t048.view_restore_ms} / ({t048.base_restore_ms} + {t048.view_apply_ms})")

block("t048-corpus-counts", "bench/RESULTS-adapters.md", [
    "The court ingested **{{t048.adapters}}** adapters and merged **{{t048.merges}}** of them.",
])
block("t048-results", "bench/RESULTS-adapters.md", [
    "| Arm | Stored bytes | Comparator | Note |",
    "|---|---:|---:|---|",
    "| Adapters only | {{t048.adapter_physical_b}} | {{t048.adapter_zstd_b}} (`zstd -19`) | {{t048.adapter_vs_zstd_pct}} % fewer; logical payload {{t048.adapter_logical_b}} B |",
    "| Base checkpoint | {{t048.base_physical_b}} | {{t048.base_zstd_b}} (`zstd -19`) | {{t048.base_vs_zstd_pct}} % fewer |",
    "| Base plus all adapters | {{t048.family_physical_b}} | {{t048.native_bytes}} (plain files) | {{t048.family_vs_native_pct}} % fewer than the files |",
    "| Merged views (incremental) | {{t048.views_incremental_b}} | — | {{t048.views_logical_b}} B of merged logical bytes in {{t048.merges}} views |",
    "| One full merged model, fresh store | {{t048.full_standalone_b}} | — | the no-graph alternative |",
    "| Same full merged model, same store | {{t048.full_shared_b}} | — | content-addressed dedup, {{t048.full_shared_ratio}}× smaller increment |",
])
block("t048-time", "bench/RESULTS-adapters.md", [
    "| Timing | Value |",
    "|---|---:|",
    "| Base write | {{t048.base_write_ms}} ms |",
    "| Base restore (whole container, SHA sink) | {{t048.base_restore_ms}} ms |",
    "| Merged view ({{t048.view_targets}} targets): merge | {{t048.view_merge_ms}} ms |",
    "| … apply to the restored base in memory | {{t048.view_apply_ms}} ms |",
    "| … restore the view | {{t048.view_restore_ms}} ms |",
    "| A 48-target view: restore | {{t048.aneesa_restore_ms}} ms |",
    "| Full merged model: write (fresh store) | {{t048.full_write_ms}} ms |",
    "| Full merged model: restore | {{t048.full_restore_ms}} ms |",
])
block("t048-reference", "bench/RESULTS-adapters.md", [
    "On `model.layers.0.mlp.down_proj.weight` (**{{t048.ref_elements}}** elements), NumPy's",
    "float32 matmul differs from the pinned Rust arithmetic in **{{t048.ref_mismatch_f32}}**",
    "element after bf16 rounding; a float64 accumulation differs in",
    "**{{t048.ref_mismatch_f64}}** elements. The largest difference is",
    "**{{t048.ref_max_abs}}** in absolute value. Both implementations compute the same",
    "real-valued expression; neither output is the other's bytes.",
], inline=True)
block("t048-losses", "bench/RESULTS-adapters.md", [
    "* Small adapters lose to `zstd -19`: the `Hazde/careerbot…` adapter stores **{{t048.small_physical_b}}** B against **{{t048.small_zstd_b}}** B (**{{t048.small_ratio}}×**), because manifest and frame overhead dominate a {{t048.small_source_b}}-byte file.",
    "* The merged-view family costs **{{t048.views_incremental_b}}** B incremental; its total (**{{t048.views_family_b}}** B) is **{{t048.views_vs_base_plus_full}}×** the base plus one standalone merged model.",
    "* A merged-view restore is not automatically faster: the first view restores in **{{t048.view_restore_ms}}** ms versus **{{t048.base_restore_ms}}** ms for the base plus **{{t048.view_apply_ms}}** ms to apply the adapter in memory (**{{t048.view_vs_apply}}×**), so the view wins only when a loader would otherwise do both. A 48-target view restored in **{{t048.aneesa_restore_ms}}** ms under page-cache pressure.",
    "* The largest adapter stores **{{t048.big_physical_b}}** B versus **{{t048.big_zstd_b}}** B for `zstd -19` (**{{t048.big_ratio}}×**) on a {{t048.big_source_b}}-byte file.",
])

# ---------------------------------------------------------------- T049
n("t049.model_tensors", "Model parameters", "", "t049", "int", R49, "model.tensors", S49, V49)
n("t049.model_physical_b", "Model checkpoint stored", "B", "t049", "int", R49, "model.physical_bytes", S49, V49)
n("t049.step", "AdamW step recorded", "", "t049", "int", R49, "optimizer_state.adamw.step", S49, V49)
n("t049.loss", "Cross-entropy loss", "", "t049", "fixed:6", R49, "gradients.loss", S49, V49)
n("t049.global_norm", "Gradient global norm", "", "t049", "fixed:6", R49, "gradients.global_norm", S49, V49)
n("t049.opt_logical_b", "Optimizer moments logical", "B", "t049", "int", R49,
  "optimizer_state.logical_bytes", S49, V49)
n("t049.opt_physical_b", "Optimizer state stored (all metadata)", "B", "t049", "int", R49,
  "optimizer_state.stored_physical_bytes", S49, V49)
n("t049.opt_zstd_b", "Optimizer moments under `zstd -19`", "B", "t049", "int", R49,
  "optimizer_state.zstd19_raw_bytes", S49, V49)
n("t049.opt_manifest_b", "Optimizer-state manifest", "B", "t049", "int", R49,
  "optimizer_state.manifest_bytes", S49, V49)
n("t049.opt_write_ms", "Optimizer-state write", "ms", "t049", "fixed:1", R49,
  "optimizer_state.write_ms", S49, V49)
n("t049.opt_restore_ms", "Optimizer-state restore", "ms", "t049", "fixed:1", R49,
  "optimizer_state.restore_ms", S49, V49)
n("t049.opt_mode9", "Optimizer tensors stored as mode 9", "", "t049", "int", R49,
  "optimizer_state.strategy_counts.float-tensor", S49, V49)
n("t049.opt_raw", "Optimizer tensors stored raw", "", "t049", "int", R49,
  "optimizer_state.strategy_counts.raw", S49, V49)
n("t049.opt_zstd_count", "Optimizer tensors stored as zstd", "", "t049", "int", R49,
  "optimizer_state.strategy_counts.zstd", S49, V49)
n("t049.grad_logical_b", "Gradients logical", "B", "t049", "int", R49, "gradients.logical_bytes", S49, V49)
n("t049.grad_physical_b", "Gradients stored", "B", "t049", "int", R49,
  "gradients.stored_physical_bytes", S49, V49)
n("t049.grad_zstd_b", "Gradients under `zstd -19`", "B", "t049", "int", R49,
  "gradients.zstd19_raw_bytes", S49, V49)
n("t049.grad_write_ms", "Gradient write", "ms", "t049", "fixed:1", R49, "gradients.write_ms", S49, V49)
n("t049.grad_restore_ms", "Gradient restore", "ms", "t049", "fixed:1", R49, "gradients.restore_ms", S49, V49)
n("t049.mask_logical_b", "Mask logical", "B", "t049", "int", R49, "attention_mask.logical_bytes", S49, V49)
n("t049.mask_physical_b", "Mask stored", "B", "t049", "int", R49,
  "attention_mask.stored_physical_bytes", S49, V49)
n("t049.mask_zstd_b", "Mask under `zstd -19`", "B", "t049", "int", R49,
  "attention_mask.zstd19_raw_bytes", S49, V49)
n("t049.mask_manifest_b", "Attention-mask manifest", "B", "t049", "int", R49,
  "attention_mask.manifest_bytes", S49, V49)
n("t049.mask_write_ms", "Attention-mask write", "ms", "t049", "fixed:1", R49,
  "attention_mask.write_ms", S49, V49)
n("t049.mask_restore_ms", "Attention-mask restore", "ms", "t049", "fixed:3", R49,
  "attention_mask.restore_ms", S49, V49)
n("t049.total_logical_b", "Three artifacts logical", "B", "t049", "int", R49,
  "training_state_total.logical_bytes", S49, V49)
n("t049.total_physical_b", "Three artifacts stored", "B", "t049", "int", R49,
  "training_state_total.stored_physical_bytes", S49, V49)
n("t049.total_zstd_b", "Three artifacts under `zstd -19`", "B", "t049", "int", R49,
  "training_state_total.zstd19_total_bytes", S49, V49)
n("t049.go_representations", "WeftFS-Go `verify --full` representations", "", "t049", "int", R49,
  "go_verify.report.CheckedRepresentations", S49, V49)
n("t049.opt_vs_zstd_pct", "Optimizer state below `zstd -19`", "%", "t049", "pct:2",
  expr="(1 - {t049.opt_physical_b} / {t049.opt_zstd_b}) * 100")
n("t049.opt_vs_raw_pct", "Optimizer state below its raw bytes", "%", "t049", "pct:2",
  expr="(1 - {t049.opt_physical_b} / {t049.opt_logical_b}) * 100")
n("t049.grad_vs_zstd_pct", "Gradients below `zstd -19`", "%", "t049", "pct:2",
  expr="(1 - {t049.grad_physical_b} / {t049.grad_zstd_b}) * 100")
n("t049.mask_vs_zstd_ratio", "Mask stored / `zstd -19`", "x", "t049", "ratio:1",
  expr="{t049.mask_physical_b} / {t049.mask_zstd_b}")
n("t049.total_vs_zstd_pct", "All three artifacts below `zstd -19`", "%", "t049", "pct:2",
  expr="(1 - {t049.total_physical_b} / {t049.total_zstd_b}) * 100")

block("t049-shape", "bench/RESULTS-training-state.md", [
    "The fixture has **{{t049.model_tensors}}** parameters, an AdamW step of **{{t049.step}}**,",
    "a cross-entropy loss of **{{t049.loss}}** and a global gradient norm of",
    "**{{t049.global_norm}}**.",
], inline=True)
block("t049-results", "bench/RESULTS-training-state.md", [
    "| Artifact | Logical bytes | Stored bytes | `zstd -19` | Stored vs zstd | Write ms | Restore ms |",
    "|---|---:|---:|---:|---:|---:|---:|",
    "| Optimizer state (f32 moments, explicit dtype) | {{t049.opt_logical_b}} | {{t049.opt_physical_b}} | {{t049.opt_zstd_b}} | {{t049.opt_vs_zstd_pct}} % smaller | {{t049.opt_write_ms}} | {{t049.opt_restore_ms}} |",
    "| Gradients (bf16, explicit dtype) | {{t049.grad_logical_b}} | {{t049.grad_physical_b}} | {{t049.grad_zstd_b}} | {{t049.grad_vs_zstd_pct}} % smaller | {{t049.grad_write_ms}} | {{t049.grad_restore_ms}} |",
    "| Attention mask (u8, causal + padding) | {{t049.mask_logical_b}} | {{t049.mask_physical_b}} | {{t049.mask_zstd_b}} | {{t049.mask_vs_zstd_ratio}}× larger | {{t049.mask_write_ms}} | {{t049.mask_restore_ms}} |",
    "| Total | {{t049.total_logical_b}} | {{t049.total_physical_b}} | {{t049.total_zstd_b}} | {{t049.total_vs_zstd_pct}} % smaller | — | — |",
])
block("t049-go", "bench/RESULTS-training-state.md", [
    "WeftFS-Go `verify --full` over the same store decoded",
    "**{{t049.go_representations}}** representations with no problems, which is the",
    "cross-language check for the frames these artifacts use.",
], inline=True)
block("t049-losses", "bench/RESULTS-training-state.md", [
    "* The attention mask loses badly to plain `zstd -19`: **{{t049.mask_physical_b}}** B stored versus **{{t049.mask_zstd_b}}** B — **{{t049.mask_vs_zstd_ratio}}×** — because a {{t049.mask_manifest_b}}-byte manifest and a fixed frame header dominate an {{t049.mask_logical_b}}-byte, almost-all-zero tensor. The planner never offers level 19 below 1 MiB by default.",
    "* Optimizer state is only mildly compressible: **{{t049.opt_physical_b}}** B for {{t049.opt_logical_b}} B of moments (**{{t049.opt_vs_raw_pct}}** % saved) and **{{t049.opt_vs_zstd_pct}}** % below `zstd -19`. The planner chose mode 9 for {{t049.opt_mode9}} of the moment tensors, raw for {{t049.opt_raw}}, zstd for {{t049.opt_zstd_count}}. This is not a dramatic win and is not presented as one.",
    "* Fixed costs are visible at this size: a {{t049.grad_logical_b}}-byte gradient artifact takes {{t049.grad_write_ms}} ms to write and {{t049.opt_manifest_b}}-byte manifests are part of every stored total.",
])

# ---------------------------------------------------------------- T051
n("t051.accepts", "Accept decisions", "", "t051", "int", R51, "summary.accepted", S51, V51)
n("t051.declines", "Decline decisions", "", "t051", "int", R51, "summary.declined", S51, V51)
n("t051.p01_target_b", "pythia step0->step1 target bytes", "B", "t051", "int", R51,
  "cells.0.target_bytes", S51, V51)
n("t051.p01_base_b", "pythia step0->step1 base bytes", "B", "t051", "int", R51,
  "cells.0.base_bytes", S51, V51)
n("t051.p01_frame_b", "pythia step0->step1 DELTA frame", "B", "t051", "int", R51,
  "cells.0.frame_bytes", S51, V51)
n("t051.p01_matched_frac", "pythia step0->step1 byte-identical fraction", "", "t051", "fixed:6", R51,
  "cells.0.matched_fraction", S51, V51)
n("t051.p01_patch_b", "pythia step0->step1 `zstd --patch-from -19`", "B", "t051", "int", R51,
  "cells.0.patch_from.bytes", S51, V51)
n("t051.p01_store_b", "pythia step0->step1 planner marginal", "B", "t051", "int", R51,
  "cells.0.store.target_marginal_bytes", S51, V51)
n("t051.p01_zstd_b", "pythia step0->step1 independent `zstd -19`", "B", "t051", "int", R51,
  "cells.0.zstd19_target_bytes", S51, V51)
n("t051.p08_base_b", "pythia step0->step8 base bytes", "B", "t051", "int", R51,
  "cells.1.base_bytes", S51, V51)
n("t051.p08_target_b", "pythia step0->step8 target bytes", "B", "t051", "int", R51,
  "cells.1.target_bytes", S51, V51)
n("t051.qwen_slice_b", "Qwen fine-tune slice length", "B", "t051", "int", R51,
  "cells.2.slice_bytes", S51, V51)
n("t051.cross_matched_pct", "Cross-size/dtype byte-identical fraction", "%", "t051", "pct:6", R51,
  "cells.4.matched_fraction", S51, V51)
n("t051.same_shape_matched_pct", "Same-dtype/different-shape byte-identical fraction", "%", "t051", "pct:6", R51,
  "cells.5.matched_fraction", S51, V51)
n("t051.synthetic_matched_pct", "Synthetic pair byte-identical fraction", "%", "t051", "pct:6", R51,
  "cells.6.matched_fraction", S51, V51)
n("t051.p08_matched_frac", "pythia step0->step8 byte-identical fraction", "", "t051", "fixed:6", R51,
  "cells.1.matched_fraction", S51, V51)
n("t051.p08_patch_b", "pythia step0->step8 `zstd --patch-from -19`", "B", "t051", "int", R51,
  "cells.1.patch_from.bytes", S51, V51)
n("t051.p08_zstd_b", "pythia step0->step8 independent `zstd -19`", "B", "t051", "int", R51,
  "cells.1.zstd19_target_bytes", S51, V51)
n("t051.p08_wd_b", "pythia step0->step8 forced weight-delta marginal", "B", "t051", "int", R51,
  "cells.1.forced_arms.0.target_marginal_bytes", S51, V51)
n("t051.qwen_slice_matched", "Qwen fine-tune slice byte-identical fraction", "", "t051", "fixed:12", R51,
  "cells.2.matched_fraction", S51, V51)
n("t051.qwen_slice_patch_b", "Qwen fine-tune slice `zstd --patch-from -19`", "B", "t051", "int", R51,
  "cells.2.patch_from.bytes", S51, V51)
n("t051.qwen_slice_zstd_b", "Qwen fine-tune slice independent `zstd -19`", "B", "t051", "int", R51,
  "cells.2.zstd19_target_bytes", S51, V51)
n("t051.qwen_base_b", "Qwen store arm base physical", "B", "t051", "int", R51,
  "cells.3.store.base_physical_bytes", S51, V51)
n("t051.qwen_store_b", "Qwen store arm target marginal", "B", "t051", "int", R51,
  "cells.3.store.target_marginal_bytes", S51, V51)
n("t051.qwen_zstd_b", "Qwen instruct independent `zstd -19`", "B", "t051", "int", R51,
  "cells.3.zstd19_target_bytes", S51, V51)
n("t051.qwen_wd_tensors", "Qwen weight-delta tensors", "", "t051", "int", R51,
  "cells.3.store.strategy_counts.weight-delta", S51, V51)
n("t051.cross_base_b", "Cross-size/dtype base tensor bytes", "B", "t051", "int", R51,
  "cells.4.base_bytes", S51, V51)
n("t051.cross_target_b", "Cross-size/dtype target tensor bytes", "B", "t051", "int", R51,
  "cells.4.target_bytes", S51, V51)
n("t051.same_shape_base_b", "Same-dtype/different-shape base bytes", "B", "t051", "int", R51,
  "cells.5.base_bytes", S51, V51)
n("t051.same_shape_target_b", "Same-dtype/different-shape target bytes", "B", "t051", "int", R51,
  "cells.5.target_bytes", S51, V51)
n("t051.synthetic_b", "Synthetic nothing-in-common pair bytes", "B", "t051", "int", R51,
  "cells.6.target_bytes", S51, V51)
n("t051.p01_matched_pct", "pythia step0->step1 byte-identical fraction", "%", "t051", "pct:3",
  expr="{t051.p01_matched_frac} * 100")
n("t051.p08_matched_pct", "pythia step0->step8 byte-identical fraction", "%", "t051", "pct:3",
  expr="{t051.p08_matched_frac} * 100")
n("t051.qwen_slice_matched_pct", "Qwen fine-tune slice byte-identical fraction", "%", "t051", "pct:6",
  expr="{t051.qwen_slice_matched} * 100")
n("t051.p01_frame_vs_patch", "`zstd --patch-from` / byte delta frame", "x", "t051", "ratio:2",
  expr="{t051.p01_patch_b} / {t051.p01_frame_b}")
n("t051.p01_store_vs_frame", "Planner marginal / byte delta frame", "x", "t051", "ratio:2",
  expr="{t051.p01_store_b} / {t051.p01_frame_b}")
n("t051.p08_patch_vs_wd", "`zstd --patch-from` / forced weight-delta", "x", "t051", "ratio:2",
  expr="{t051.p08_patch_b} / {t051.p08_wd_b}")
n("t051.qwen_store_vs_zstd", "Independent `zstd -19` / planner marginal", "x", "t051", "ratio:2",
  expr="{t051.qwen_zstd_b} / {t051.qwen_store_b}")

block("t051-summary", "bench/RESULTS-cross-model-delta.md", [
    "**{{t051.accepts}}** pair is accepted and **{{t051.declines}}** are declined with a",
    "typed reason. The declines are the result, not a gap.",
], inline=True)
block("t051-table", "bench/RESULTS-cross-model-delta.md", [
    "| Pair | Base / target bytes | Byte-identical | DELTA frame | `--patch-from` | Planner or forced | Independent `zstd -19` |",
    "|---|---:|---:|---:|---:|---:|---:|",
    "| pythia step0 → step1 (F32, same shape) | {{t051.p01_base_b}} / {{t051.p01_target_b}} | {{t051.p01_matched_pct}} % | {{t051.p01_frame_b}} | {{t051.p01_patch_b}} | {{t051.p01_store_b}} | {{t051.p01_zstd_b}} |",
    "| pythia step0 → step8 (F32, same shape) | {{t051.p08_base_b}} / {{t051.p08_target_b}} | {{t051.p08_matched_pct}} % | declined | {{t051.p08_patch_b}} | {{t051.p08_wd_b}} | {{t051.p08_zstd_b}} |",
    "| Qwen base → Instruct, {{t051.qwen_slice_b}}-byte slice | {{t051.qwen_slice_b}} / {{t051.qwen_slice_b}} | {{t051.qwen_slice_matched_pct}} % | declined | {{t051.qwen_slice_patch_b}} | — | {{t051.qwen_slice_zstd_b}} |",
    "| Qwen base → Instruct, whole model (store arm) | {{t051.qwen_base_b}} / — | not buildable in 3 GiB | declined | — | {{t051.qwen_store_b}} | {{t051.qwen_zstd_b}} |",
    "| Qwen 0.5B BF16 vs pythia-70m F32 embedding | {{t051.cross_base_b}} / {{t051.cross_target_b}} | {{t051.cross_matched_pct}} % | declined | — | ineligible (dtype) | — |",
    "| Two Qwen BF16 tensors, different shapes | {{t051.same_shape_base_b}} / {{t051.same_shape_target_b}} | {{t051.same_shape_matched_pct}} % | declined | — | ineligible (shape) | — |",
    "| SYNTHETIC nothing-in-common pair | {{t051.synthetic_b}} / {{t051.synthetic_b}} | {{t051.synthetic_matched_pct}} % | declined | — | — | — |",
])
block("t051-reading", "bench/RESULTS-cross-model-delta.md", [
    "On the one pair with byte-identical regions the frame is",
    "**{{t051.p01_frame_vs_patch}}×** smaller than `zstd --patch-from` and",
    "**{{t051.p01_store_vs_frame}}×** smaller than the shipped planner's marginal bytes.",
    "On the distant pair the planner's forced weight-delta arm is",
    "**{{t051.p08_patch_vs_wd}}×** smaller than `zstd --patch-from`, and the whole",
    "Qwen fine-tune stores **{{t051.qwen_store_vs_zstd}}×** below independent `zstd -19`",
    "with **{{t051.qwen_wd_tensors}}** weight-delta tensors. Those are value-level",
    "deltas; the byte-level delta correctly refuses to pretend it can do the same.",
], inline=True)
block("t051-losses", "bench/RESULTS-cross-model-delta.md", [
    "* Byte-identical structural reuse is essentially absent between independent fine-tunes: the Qwen slice matched **{{t051.qwen_slice_matched_pct}}** % of bytes, so the mode declines rather than emitting a nearly-full INSERT stream.",
    "* Even consecutive checkpoints only help the byte delta when almost nothing changed: step0 → step1 matched **{{t051.p01_matched_pct}}** %, while step0 → step8 matched **{{t051.p08_matched_pct}}** % and declined.",
    "* The forced sparse-delta arm is refused on `embed_out` (`LimitExceeded`, 22,339,018 non-zero runs against a 16,777,216 bound); that is a recorded refusal, not a fallback.",
    "* The whole-model Qwen byte delta cannot be built inside the 3 GiB court (two 988 MB inputs plus a frame), so that pair is a slice plus a whole-model store arm, stated in every cell.",
])

# ---------------------------------------------------------------- T052
n("t052.down_exact_b", "down_proj exact stored", "B", "t052", "int", R52,
  "tensors.0.exact_stored_physical_bytes", S52, V52)
n("t052.down_exact_read_ms", "down_proj exact read", "ms", "t052", "fixed:2", R52,
  "tensors.0.exact_read_ms", S52, V52)
n("t052.down_b32_b", "down_proj view (block 32) stored", "B", "t052", "int", R52,
  "tensors.0.views.0.stored_physical_bytes", S52, V52)
n("t052.down_b32_ratio", "down_proj view (block 32) / exact", "x", "t052", "ratio:3", R52,
  "tensors.0.views.0.physical_vs_exact", S52, V52)
n("t052.down_b128_b", "down_proj view (block 128) stored", "B", "t052", "int", R52,
  "tensors.0.views.2.stored_physical_bytes", S52, V52)
n("t052.down_b128_ratio", "down_proj view (block 128) / exact", "x", "t052", "ratio:3", R52,
  "tensors.0.views.2.physical_vs_exact", S52, V52)
n("t052.down_b128_id_ms", "down_proj view identity-only read", "ms", "t052", "fixed:2", R52,
  "tensors.0.views.2.view_read_identity_only_ms", S52, V52)
n("t052.down_b128_strict_ms", "down_proj view strict read", "ms", "t052", "fixed:2", R52,
  "tensors.0.views.2.view_read_ms", S52, V52)
n("t052.down_err", "down_proj measured max error", "", "t052", "fixed:9", R52,
  "tensors.0.views.2.max_abs_error", S52, V52)
n("t052.down_bound", "down_proj declared bound", "", "t052", "fixed:9", R52,
  "tensors.0.views.2.max_bound", S52, V52)
n("t052.embed_exact_b", "embedding exact stored", "B", "t052", "int", R52,
  "tensors.1.exact_stored_physical_bytes", S52, V52)
n("t052.embed_exact_read_ms", "embedding exact read", "ms", "t052", "fixed:2", R52,
  "tensors.1.exact_read_ms", S52, V52)
n("t052.embed_b128_b", "embedding view (block 128) stored", "B", "t052", "int", R52,
  "tensors.1.views.2.stored_physical_bytes", S52, V52)
n("t052.embed_b128_ratio", "embedding view (block 128) / exact", "x", "t052", "ratio:3", R52,
  "tensors.1.views.2.physical_vs_exact", S52, V52)
n("t052.embed_b128_id_ms", "embedding view identity-only read", "ms", "t052", "fixed:2", R52,
  "tensors.1.views.2.view_read_identity_only_ms", S52, V52)
n("t052.embed_err", "embedding measured max error", "", "t052", "fixed:9", R52,
  "tensors.1.views.2.max_abs_error", S52, V52)
n("t052.embed_bound", "embedding declared bound", "", "t052", "fixed:9", R52,
  "tensors.1.views.2.max_bound", S52, V52)
n("t052.pythia_exact_b", "pythia tensor exact stored", "B", "t052", "int", R52,
  "pythia_cells.0.exact_stored_physical_bytes", S52, V52)
n("t052.pythia_exact_read_ms", "pythia tensor exact read", "ms", "t052", "fixed:2", R52,
  "pythia_cells.0.exact_read_ms", S52, V52)
n("t052.pythia_b128_b", "pythia tensor view (block 128) stored", "B", "t052", "int", R52,
  "pythia_cells.0.views.2.stored_physical_bytes", S52, V52)
n("t052.pythia_b128_ratio", "pythia tensor view (block 128) / exact", "x", "t052", "ratio:3", R52,
  "pythia_cells.0.views.2.physical_vs_exact", S52, V52)
n("t052.pythia_b128_id_ms", "pythia tensor view identity-only read", "ms", "t052", "fixed:2", R52,
  "pythia_cells.0.views.2.view_read_identity_only_ms", S52, V52)
n("t052.pythia_err", "pythia measured max error", "", "t052", "fixed:9", R52,
  "pythia_cells.0.views.2.max_abs_error", S52, V52)
n("t052.pythia_bound", "pythia declared bound", "", "t052", "fixed:9", R52,
  "pythia_cells.0.views.2.max_bound", S52, V52)
n("t052.down_saved_pct", "down_proj bytes saved (block 128)", "%", "t052", "pct:2",
  expr="(1 - {t052.down_b128_ratio}) * 100")
n("t052.embed_saved_pct", "embedding bytes saved (block 128)", "%", "t052", "pct:2",
  expr="(1 - {t052.embed_b128_ratio}) * 100")
n("t052.pythia_saved_pct", "pythia tensor bytes saved (block 128)", "%", "t052", "pct:2",
  expr="(1 - {t052.pythia_b128_ratio}) * 100")
n("t052.down_id_vs_exact", "down_proj identity-only read / exact read", "x", "t052", "ratio:2",
  expr="{t052.down_b128_id_ms} / {t052.down_exact_read_ms}")
n("t052.down_strict_vs_exact", "down_proj strict read / exact read", "x", "t052", "ratio:2",
  expr="{t052.down_b128_strict_ms} / {t052.down_exact_read_ms}")
n("t052.embed_id_vs_exact", "embedding identity-only read / exact read", "x", "t052", "ratio:2",
  expr="{t052.embed_b128_id_ms} / {t052.embed_exact_read_ms}")
n("t052.pythia_id_vs_exact", "pythia identity-only read / exact read", "x", "t052", "ratio:2",
  expr="{t052.pythia_b128_id_ms} / {t052.pythia_exact_read_ms}")

block("t052-table", "bench/RESULTS-view-identity.md", [
    "| Tensor | Exact stored | View b32 | View b128 | Exact read / view identity-only / view strict (ms, b128) |",
    "|---|---:|---:|---:|---:|",
    "| Qwen `down_proj` (bf16) | {{t052.down_exact_b}} | {{t052.down_b32_b}} ({{t052.down_b32_ratio}}×) | {{t052.down_b128_b}} ({{t052.down_b128_ratio}}×) | {{t052.down_exact_read_ms}} / {{t052.down_b128_id_ms}} / {{t052.down_b128_strict_ms}} |",
    "| Qwen embedding (bf16) | {{t052.embed_exact_b}} | — | {{t052.embed_b128_b}} ({{t052.embed_b128_ratio}}×) | {{t052.embed_exact_read_ms}} / {{t052.embed_b128_id_ms}} / — |",
    "| pythia QKV (f32) | {{t052.pythia_exact_b}} | — | {{t052.pythia_b128_b}} ({{t052.pythia_b128_ratio}}×) | {{t052.pythia_exact_read_ms}} / {{t052.pythia_b128_id_ms}} / — |",
])
block("t052-error", "bench/RESULTS-view-identity.md", [
    "| Tensor | Measured max abs error | Declared bound |",
    "|---|---:|---:|",
    "| Qwen `down_proj` | {{t052.down_err}} | {{t052.down_bound}} |",
    "| Qwen embedding | {{t052.embed_err}} | {{t052.embed_bound}} |",
    "| pythia QKV | {{t052.pythia_err}} | {{t052.pythia_bound}} |",
])
block("t052-verdict", "bench/RESULTS-view-identity.md", [
    "* Bytes: the view wins — **{{t052.down_saved_pct}}** % fewer stored bytes on the",
    "  Qwen MLP matrix, **{{t052.embed_saved_pct}}** % on the embedding and",
    "  **{{t052.pythia_saved_pct}}** % on the f32 pythia matrix (block 128).",
    "* Read time: the view loses — **{{t052.down_id_vs_exact}}×** the exact read with",
    "  identity checking only, and **{{t052.down_strict_vs_exact}}×** when the strict door",
    "  re-reads the source and re-measures the bound. The embedding shows the same",
    "  shape (**{{t052.embed_id_vs_exact}}×**) and so does the f32 tensor",
    "  (**{{t052.pythia_id_vs_exact}}×**).",
    "* Verdict: a q8_0 view is a **bandwidth and capacity** representation, not a",
    "  latency representation. Larger blocks trade a little more error for fewer",
    "  bytes; the error stays inside the declared bound in every cell.",
])

manifest = json.loads(MANIFEST.read_text())
existing_groups = {group["id"] for group in manifest.get("groups", [])}
for group in GROUPS:
    if group["id"] not in existing_groups:
        manifest["groups"].append(group)
existing_keys = {entry["key"] for entry in manifest.get("numbers", [])}
for entry in NUMBERS:
    if entry["key"] not in existing_keys:
        manifest["numbers"].append(entry)
existing_blocks = {entry["id"] for entry in manifest.get("blocks", [])}
for entry in BLOCKS:
    if entry["id"] not in existing_blocks:
        manifest["blocks"].append(entry)
MANIFEST.write_text(json.dumps(manifest, indent=1) + "\n")
print("groups", len(manifest["groups"]), "numbers", len(manifest["numbers"]), "blocks", len(manifest["blocks"]))
