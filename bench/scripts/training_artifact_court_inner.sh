#!/usr/bin/env bash
# Inner half of the P01/P02 Training Artifact court. Runs inside the fuse court
# (Rust + Go), never on the host. The outer script is
# `scripts/training_artifact_court.sh`.
#
#   bash bench/scripts/training_artifact_court_inner.sh <stamp>
set -Eeuo pipefail
cd /work

stamp="${1:?stamp}"
work=".docker-cache/training-artifact-${stamp}"
mkdir -p "$work" bench/receipts
rm -rf "$work"/*
log="bench/receipts/training-artifact-${stamp}.log"
: > "$log"

say() { echo "$@" | tee -a "$log"; }

say "== build Rust fixture, CLI and Go consumer =="
cargo build --release --quiet --example training_artifact_fixture
cargo build --release --quiet --bin weftfs
weft="./.docker-cache/target-rust/release/weftfs"
fixture="./.docker-cache/target-rust/release/examples/training_artifact_fixture"
(cd WeftFS-Go && go build -o "/work/$work/go-ta" ./cmd/training-artifact) | tee -a "$log"

say "== fixture (Rust writes) =="
"$fixture" init "$work/rust" | tee "$work/rust.json" | tee -a "$log"
root="$(python3 -c "import json;print(json.load(open('$work/rust.json'))['artifact_root'])")"
snapshot="$(python3 -c "import json;print(json.load(open('$work/rust.json'))['snapshot'])")"

say "== Rust resolves and validates =="
"$weft" --store "$work/rust/store" --json training-artifact validate \
  --root "$root" --snapshot "$snapshot" > "$work/rust-validate.json" 2>>"$log"

say "== Go resolves and validates the same artifact =="
"$work/go-ta" validate --store "$work/rust/store" --root "$root" \
  --snapshot "$snapshot" > "$work/go-validate.json" 2>>"$log"

say "== negative cases =="
for case in policy lineage missing value; do
  cp -a "$work/rust" "$work/$case"
  "$fixture" "tamper-$case" "$work/$case" "$root" > "$work/$case.json" 2>>"$log"
  tampered="$(python3 -c "import json;print(json.load(open('$work/$case.json'))['artifact_root'])")"
  set +e
  rust_out="$("$weft" --store "$work/$case/store" --json training-artifact validate \
    --root "$tampered" 2>&1)"
  rust_rc=$?
  python3 - "$work/rust-$case.json" "$rust_rc" "$rust_out" <<'PYREC'
import json, sys
path, rc, text = sys.argv[1], int(sys.argv[2]), sys.argv[3]
class_name = text.split(":", 1)[0].strip()
json.dump({"ok": rc == 0, "class": class_name, "message": text.strip()},
          open(path, "w"), indent=2, sort_keys=True)
PYREC
  "$work/go-ta" validate --store "$work/$case/store" --root "$tampered" \
    > "$work/go-$case.json" 2>/dev/null
  go_rc=$?
  set -e
  echo "$case rust_rc=$rust_rc go_rc=$go_rc" | tee -a "$log"
done

say "== grant edits never change the artifact root =="
cp -a "$work/rust" "$work/grant"
"$fixture" grant "$work/grant" "$root" > "$work/rust-grant.json" 2>>"$log"
"$work/go-ta" grants --store "$work/grant/store" --root "$root" > "$work/go-grants-1.json" 2>>"$log"
"$work/go-ta" grant --store "$work/grant/store" --root "$root" --uid 4242 --gid 4242 \
  --actions evaluate,train > "$work/go-grant.json" 2>>"$log"
"$work/go-ta" grants --store "$work/grant/store" --root "$root" > "$work/go-grants-2.json" 2>>"$log"
"$weft" --store "$work/grant/store" --json training-artifact validate \
  --root "$root" --check-grants --now-ns 1700000000000000001 \
  > "$work/rust-grant-validate.json" 2>>"$log"

say "== Go writes, Rust and Go resolve =="
go_store="$work/go-store"
mkdir -p "$go_store"
"$weft" --store "$go_store" init | tee -a "$log"
a_obj="$("$work/go-ta" write-tensor --store "$go_store" --path alpha --dtype bf16 \
  --shape 4x4 --bytes 000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["object"])')"
b_obj="$("$work/go-ta" write-tensor --store "$go_store" --path beta --dtype f32 \
  --shape 2x2 --bytes 0000803f000000400000404000008040 \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["object"])')"
"$work/go-ta" build-tensor-set --store "$go_store" --name go-written --lineage none \
  --tensor "alpha=$a_obj" --tensor "beta=$b_obj" > "$work/go-build.json" 2>>"$log"
go_root="$(python3 -c "import json;print(json.load(open('$work/go-build.json'))['artifact_root'])")"
"$weft" --store "$go_store" --json training-artifact validate --root "$go_root" \
  > "$work/rust-on-go.json" 2>>"$log"
"$work/go-ta" validate --store "$go_store" --root "$go_root" \
  > "$work/go-on-go.json" 2>>"$log"
cp -a "$go_store" "$work/go-store-tampered"
"$work/go-ta" tamper-value --store "$work/go-store-tampered" --root "$go_root" \
  > "$work/go-tamper.json" 2>>"$log"
set +e
rust_tamper_out="$("$weft" --store "$work/go-store-tampered" --json training-artifact validate \
  --root "$go_root" 2>&1)"
go_tamper_rc=$?
set -e
python3 - "$work/rust-on-go-tampered.json" "$go_tamper_rc" "$rust_tamper_out" <<'PYREC'
import json, sys
path, rc, text = sys.argv[1], int(sys.argv[2]), sys.argv[3]
class_name = text.split(":", 1)[0].strip()
json.dump({"ok": rc == 0, "class": class_name, "message": text.strip()},
          open(path, "w"), indent=2, sort_keys=True)
PYREC
echo "go-written tampered rust_rc=$go_tamper_rc" | tee -a "$log"

say "== resolution refusal is verifiable from canonical records =="
missing_root="$(python3 -c "import json;print(json.load(open('$work/missing.json'))['artifact_root'])")"
"$fixture" closure "$work/missing" "$missing_root" complete "$work/closure-refusal.json" \
  > "$work/closure-refusal-run.json" 2>>"$log" || true
python3 verifier/closure_check.py --bundle "$work/closure-refusal.json" \
  > "$work/verify-refusal.json" 2>>"$log" || true
"$work/go-ta" closure --store "$work/missing/store" --root "$missing_root" --mode complete \
  --bundle-out "$work/go-closure-refusal.json" > /dev/null 2>>"$log" || true
python3 verifier/closure_check.py --bundle "$work/go-closure-refusal.json" \
  > "$work/verify-go-refusal.json" 2>>"$log" || true

say "== bounded closure bundles (complete and minimal) =="
"$fixture" closure "$work/rust" "$root" complete "$work/closure-complete.json" > /dev/null 2>>"$log"
"$fixture" closure "$work/rust" "$root" minimal "$work/closure-minimal.json" > /dev/null 2>>"$log"
"$work/go-ta" closure --store "$work/rust/store" --root "$root" --mode complete \
  --bundle-out "$work/go-closure.json" > /dev/null 2>>"$log"
python3 verifier/closure_check.py --bundle "$work/go-closure.json" \
  > "$work/verify-go-closure.json" 2>>"$log"

say "== assemble receipt =="
python3 - "$stamp" "$work" <<'PY'
import json
import os
import subprocess
import sys

stamp, work = sys.argv[1], sys.argv[2]


def read(name):
    path = os.path.join(work, name)
    with open(path) as fh:
        return json.load(fh)


def run_verifier(bundle):
    proc = subprocess.run(
        [sys.executable, "verifier/closure_check.py", "--bundle", bundle],
        capture_output=True, text=True)
    try:
        report = json.loads(proc.stdout)
    except json.JSONDecodeError:
        report = {"ok": False, "error": proc.stdout + proc.stderr}
    return proc.returncode, report


checks = []


def check(name, ok, detail):
    checks.append({"name": name, "ok": bool(ok), "detail": detail})


rust = read("rust-validate.json")
go = read("go-validate.json")
check("rust_validates", rust.get("ok") is True, "rust validation")
check("go_validates_rust_artifact", go.get("ok") is True, go.get("class", "ok"))
check("artifact_root_agrees",
      rust.get("artifact_root") == go.get("artifact_root") == read("rust.json")["artifact_root"],
      go.get("artifact_root"))
check("go_checked_every_tensor",
      go.get("tensors_exact") == rust.get("tensors_exact") == rust.get("tensor_count") == 1,
      "tensors_exact=%s" % go.get("tensors_exact"))
check("go_checked_checkpoint_manifest", go.get("checkpoint_verified") is True, "manifest")
check("go_checked_context_root", go.get("context_root_verified") is True, "context root")
check("go_checked_lineage_closure",
      go.get("lineage_mode") == "complete" and go.get("causal_nodes") >= 2,
      "causal_nodes=%s" % go.get("causal_nodes"))
check("go_checked_provenance",
      go.get("provenance_verified") == rust.get("provenance_verified") == 1,
      "provenance_verified=%s" % go.get("provenance_verified"))

negatives = {}
expected = {"policy": "Corrupt", "lineage": "Malformed", "missing": "DependencyMismatch",
            "value": "Corrupt"}
for case in ["policy", "lineage", "missing", "value"]:
    r = read("rust-%s.json" % case)
    g = read("go-%s.json" % case)
    negatives[case] = {
        "rust": {"ok": r.get("ok"), "class": r.get("class"), "message": r.get("message")},
        "go": {"ok": g.get("ok"), "class": g.get("class"), "message": g.get("message")},
    }
    want = expected[case]
    if case == "value":
        check("negative_%s_refused_both" % case,
              r.get("ok") is False and g.get("ok") is False and
              r.get("class") in ("Corrupt", "DependencyMismatch") and
              g.get("class") in ("Corrupt", "DependencyMismatch"),
              "rust=%s go=%s" % (r.get("class"), g.get("class")))
    else:
        check("negative_%s_refused_both" % case,
              r.get("ok") is False and g.get("ok") is False and
              r.get("class") == want and g.get("class") == want,
              "rust=%s go=%s want=%s" % (r.get("class"), g.get("class"), want))

grant_rust = read("rust-grant-validate.json")
g1 = read("go-grants-1.json")
g2 = read("go-grants-2.json")
check("grant_epoch_moves", g1.get("grant_epoch") == 1 and g2.get("grant_epoch") == 2,
      "epochs=%s,%s" % (g1.get("grant_epoch"), g2.get("grant_epoch")))
check("grant_root_unchanged",
      grant_rust.get("artifact_root") == read("rust.json")["artifact_root"] and
      g1.get("artifact_root") == g2.get("artifact_root") == read("rust.json")["artifact_root"],
      "root unchanged")
check("cross_language_grants_visible",
      len(g1.get("grants", [])) == 1 and len(g2.get("grants", [])) == 2,
      "go sees %d then %d grants" % (len(g1.get("grants", [])), len(g2.get("grants", []))))

r_go = read("rust-on-go.json")
g_go = read("go-on-go.json")
check("go_writes_rust_validates", r_go.get("ok") is True, r_go.get("class", "ok"))
check("go_writes_go_validates", g_go.get("ok") is True, g_go.get("class", "ok"))
check("go_written_root_agrees",
      r_go.get("artifact_root") == g_go.get("artifact_root") == read("go-build.json")["artifact_root"],
      r_go.get("artifact_root"))
tampered = read("rust-on-go-tampered.json")
check("go_written_tamper_refused_by_rust",
      tampered.get("ok") is False and
      tampered.get("class") in ("Corrupt", "DependencyMismatch"),
      tampered.get("class"))

refusal_report = read("verify-refusal.json")
check("verifier_refusal_bundle",
      refusal_report.get("ok") is True and
      (refusal_report.get("refusal") or {}).get("class") == "DependencyMismatch" and
      (refusal_report.get("refusal") or {}).get("identity") ==
          read("missing.json").get("missing"),
      refusal_report.get("error") or refusal_report.get("refusal"))

go_closure = read("verify-go-closure.json")
rust_closure_nodes = len(read("closure-complete.json")["result"]["nodes"])
check("verifier_go_closure",
      go_closure.get("ok") is True and
      go_closure.get("nodes") == rust_closure_nodes,
      go_closure.get("error") or "Go bundle: %s records, %s nodes" % (
          go_closure.get("records"), go_closure.get("nodes")))

go_refusal = read("verify-go-refusal.json")
check("verifier_go_refusal_bundle",
      go_refusal.get("ok") is True and
      (go_refusal.get("refusal") or {}).get("class") == "DependencyMismatch" and
      (go_refusal.get("refusal") or {}).get("identity") ==
          read("missing.json").get("missing"),
      go_refusal.get("error") or go_refusal.get("refusal"))

closure = {}
for mode in ["complete", "minimal"]:
    rc, report = run_verifier(os.path.join(work, "closure-%s.json" % mode))
    closure[mode] = {"verifier_rc": rc, "verifier": report}
    check("verifier_%s_closure" % mode, rc == 0 and report.get("ok") is True,
          report.get("error") or "checked from canonical records")

result = "pass" if all(c["ok"] for c in checks) else "fail"
receipt = {
    "kind": "training-artifact-court",
    "timestamp_utc": stamp,
    "court": "fuse (Rust + Go in one hard-limited container)",
    "environment": {
        "revisions": {
            "WeftFS": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
            "WeftCodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
            "WeftCodec-Go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
            "WeftFS-Go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
        },
        "image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
        "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
        "kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
        "limits": {
            "memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
            "cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
            "pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
        },
    },
    "fixture": read("rust.json"),
    "cross_language": {
        "rust_writes_go_validates": go,
        "go_writes_rust_validates": r_go,
        "go_writes_go_validates": g_go,
    },
    "negatives": negatives,
    "grants": {
        "rust_validate": grant_rust,
        "go_epochs": [g1.get("grant_epoch"), g2.get("grant_epoch")],
    },
    "closure": closure,
    "checks": checks,
    "summary": {
        "total": len(checks),
        "passed": sum(1 for c in checks if c["ok"]),
        "failed": sum(1 for c in checks if not c["ok"]),
    },
    "result": result,
}
with open("bench/receipts/training-artifact-%s.json" % stamp, "w") as fh:
    json.dump(receipt, fh, indent=2, sort_keys=True)
    fh.write("\n")
for c in checks:
    print(("PASS " if c["ok"] else "FAIL ") + c["name"] + ": " + str(c["detail"]))
print("result=" + result)
if result != "pass":
    sys.exit(1)
PY
