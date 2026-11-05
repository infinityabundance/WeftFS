#!/usr/bin/env bash
# Mounted-filesystem end-to-end benchmark for WeftFS (Rust/Go) against an ext4
# court path and tmpfs.
#
# Run ONLY inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_fuse_bench.sh"
#
# Cells:
#   1. Metadata: 2,000 files across 200 dirs -> create/stat/readdir/unlink.
#   2. fio: 4 KiB randwrite and 1 MiB seqwrite, numjobs=1, 256 MiB working set.
#   3. 500 random 4 KiB overwrites into a 128 MiB file (WeftFS only).
#   4. Cold mount of a 10,000-object store and the first ls of a saved
#      Context Root.
#
# Every run is three repetitions. The receipt embeds per-repetition raw
# observations and medians. No `winner` field is emitted.
#
# Honest boundaries (also written into the receipt):
#   * ext4 is the host ext4 filesystem backing the /work bind mount. It is not
#     a dedicated block device handed to the container.
#   * tmpfs is a REFERENCE ONLY: no durability and no FUSE layer.
#   * Page caches are NOT dropped: /proc/sys/vm/drop_caches is not writable in
#     the court (CapEff=0 for that operation; see bench/RESULTS-fuse-v0.5.md).
#   * fio is buffered (direct=0). Direct I/O is not used.
#   * The fio cells use a 256 MiB file layout but cap the sampled I/O volume:
#     512 KiB of 4 KiB random writes (128 I/Os) and 64 MiB of 1 MiB sequential
#     writes (64 I/Os). The Go write path costs O(file size) per write; full
#     256 MiB sampling needs ~14 minutes per Go repetition and cannot fit the
#     30-minute court. The same cap is applied to every target for comparability.
#     IOPS/percentiles are the real FUSE path; total bytes are bounded and stated.
#
# Product code is not modified. Helper programs are generated into the
# gitignored .docker-cache scratch tree.
#
# Outputs:
#   bench/receipts/fuse-bench-<UTC>.json
#   bench/receipts/fuse-bench-<UTC>.log
set -Eeuo pipefail

ROOT=/work
SCRATCH="${WEFT_FUSE_SCRATCH:-$ROOT/.docker-cache/fuse-bench}"
RUN="$SCRATCH/run"
RECEIPTS="$ROOT/bench/receipts"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
BASE="${WEFT_FUSE_RECEIPT_NAME:-fuse-bench-$STAMP}"
RECEIPT="$RECEIPTS/$BASE.json"
LOG="$RECEIPTS/$BASE.log"
RAW="$RUN/raw"
BIN="$RUN/bin"
TS=1767225600000000000
REPS="${WEFT_FUSE_REPS:-3}"
META_DIRS="${WEFT_FUSE_META_DIRS:-200}"
META_PER="${WEFT_FUSE_META_PER:-10}"
FIO_SIZE="${WEFT_FUSE_FIO_SIZE:-256M}"
RAND_IO="${WEFT_FUSE_RAND_IO:-512K}"
SEQ_IO="${WEFT_FUSE_SEQ_IO:-64M}"
OVER_BYTES="${WEFT_FUSE_OVER_BYTES:-128M}"
OVER_COUNT="${WEFT_FUSE_OVER_COUNT:-500}"

RUST_MNT="$RUN/mnt/rust"
GO_MNT="$RUN/mnt/go"
EXT4_DIR="$RUN/ext4"
TMPFS_DIR=/mnt/weft-bench/tmpfs-fuse-bench
CTX_MNT="$RUN/mnt/context"
RUST_STORE="$RUN/store-rust"
GO_STORE="$RUN/store-go"
CTX_STORE="$RUN/store-context"
MOUNT_PIDS=()
GO_STATUS="ok"

mkdir -p "$BIN" "$RAW" "$RECEIPTS" "$RUST_MNT" "$GO_MNT" "$EXT4_DIR" "$TMPFS_DIR" "$CTX_MNT"
rm -rf "$RUST_STORE" "$GO_STORE" "$CTX_STORE"
rm -f "$RAW"/*
: > "$RAW/commands.txt"
: > "$RAW/go-error.txt"

# ---- helpers (generated outside the product tree) -------------------------
mkdir -p "$SCRATCH/helpers"
cat > "$SCRATCH/helpers/metaops.go" <<'GO'
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"time"
)

func pct(sorted []int64, p float64) int64 {
	if len(sorted) == 0 {
		return 0
	}
	idx := int(float64(len(sorted)-1) * p)
	if idx < 0 {
		idx = 0
	}
	if idx >= len(sorted) {
		idx = len(sorted) - 1
	}
	return sorted[idx]
}

func summarize(name string, ns []int64) map[string]interface{} {
	s := append([]int64(nil), ns...)
	sort.Slice(s, func(i, j int) bool { return s[i] < s[j] })
	var total int64
	for _, v := range s {
		total += v
	}
	mean := 0.0
	if len(s) > 0 {
		mean = float64(total) / float64(len(s))
	}
	return map[string]interface{}{
		"op": name, "count": len(s), "total_ns": total,
		"p50_ns": pct(s, 0.50), "p95_ns": pct(s, 0.95), "p99_ns": pct(s, 0.99),
		"mean_ns": mean,
	}
}

func parseU64(s string) uint64 {
	var v uint64
	fmt.Sscanf(s, "%d", &v)
	return v
}

func meta(args []string) {
	root := args[0]
	dirs := int(parseU64(args[1]))
	per := int(parseU64(args[2]))
	if err := os.MkdirAll(root, 0o755); err != nil {
		panic(err)
	}
	dirPaths := make([]string, dirs)
	for i := 0; i < dirs; i++ {
		p := filepath.Join(root, fmt.Sprintf("d%03d", i))
		if err := os.Mkdir(p, 0o755); err != nil {
			panic(err)
		}
		dirPaths[i] = p
	}
	create := make([]int64, 0, dirs*per)
	stat := make([]int64, 0, dirs*per)
	unlink := make([]int64, 0, dirs*per)
	readdir := make([]int64, 0, dirs)
	files := make([]string, 0, dirs*per)
	for _, d := range dirPaths {
		for j := 0; j < per; j++ {
			p := filepath.Join(d, fmt.Sprintf("f%04d", j))
			t0 := time.Now()
			f, err := os.OpenFile(p, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o644)
			if err != nil {
				panic(err)
			}
			f.Close()
			create = append(create, time.Since(t0).Nanoseconds())
			files = append(files, p)
		}
	}
	for _, p := range files {
		t0 := time.Now()
		if _, err := os.Lstat(p); err != nil {
			panic(err)
		}
		stat = append(stat, time.Since(t0).Nanoseconds())
	}
	for _, d := range dirPaths {
		t0 := time.Now()
		f, err := os.Open(d)
		if err != nil {
			panic(err)
		}
		names, err := f.Readdirnames(-1)
		f.Close()
		if err != nil {
			panic(err)
		}
		if len(names) != per {
			panic(fmt.Sprintf("readdir %s: got %d want %d", d, len(names), per))
		}
		readdir = append(readdir, time.Since(t0).Nanoseconds())
	}
	for _, p := range files {
		t0 := time.Now()
		if err := os.Remove(p); err != nil {
			panic(err)
		}
		unlink = append(unlink, time.Since(t0).Nanoseconds())
	}
	for _, d := range dirPaths {
		if err := os.Remove(d); err != nil {
			panic(err)
		}
	}
	out := map[string]interface{}{
		"kind": "meta", "root": root, "dirs": dirs, "files_per_dir": per,
		"create": summarize("create", create), "stat": summarize("stat", stat),
		"readdir": summarize("readdir", readdir), "unlink": summarize("unlink", unlink),
	}
	b, _ := json.Marshal(out)
	fmt.Println(string(b))
}

func overwrite(args []string) {
	path := args[0]
	count := int(parseU64(args[1]))
	bs := int(parseU64(args[2]))
	info, err := os.Stat(path)
	if err != nil {
		panic(err)
	}
	size := info.Size()
	f, err := os.OpenFile(path, os.O_RDWR, 0)
	if err != nil {
		panic(err)
	}
	defer f.Close()
	buf := make([]byte, bs)
	for i := range buf {
		buf[i] = byte(i*31 + 7)
	}
	max := size - int64(bs)
	if max < 0 {
		max = 0
	}
	var state uint64 = 0x9E3779B97F4A7C15
	rand := func() uint64 {
		state ^= state << 13
		state ^= state >> 7
		state ^= state << 17
		return state
	}
	total := make([]int64, 0, count)
	syncs := make([]int64, 0, count)
	for i := 0; i < count; i++ {
		off := int64(rand() % uint64(max+1))
		t0 := time.Now()
		if _, err := f.WriteAt(buf, off); err != nil {
			panic(err)
		}
		t1 := time.Now()
		if err := f.Sync(); err != nil {
			panic(err)
		}
		t2 := time.Now()
		total = append(total, t2.Sub(t0).Nanoseconds())
		syncs = append(syncs, t2.Sub(t1).Nanoseconds())
	}
	out := map[string]interface{}{
		"kind": "overwrite", "path": path, "file_size": size, "count": count, "block_bytes": bs,
		"total": summarize("overwrite", total), "sync": summarize("sync", syncs),
	}
	b, _ := json.Marshal(out)
	fmt.Println(string(b))
}

func readdirOne(args []string) {
	dir := args[0]
	t0 := time.Now()
	f, err := os.Open(dir)
	if err != nil {
		panic(err)
	}
	names, err := f.Readdirnames(-1)
	f.Close()
	if err != nil {
		panic(err)
	}
	ns := time.Since(t0).Nanoseconds()
	out := map[string]interface{}{"kind": "readdir_one", "dir": dir, "entries": len(names), "ns": ns}
	b, _ := json.Marshal(out)
	fmt.Println(string(b))
}

func main() {
	if len(os.Args) < 2 {
		panic("usage: metaops <meta|overwrite|readdir-one> ...")
	}
	switch os.Args[1] {
	case "meta":
		meta(os.Args[2:])
	case "overwrite":
		overwrite(os.Args[2:])
	case "readdir-one":
		readdirOne(os.Args[2:])
	default:
		panic("unknown mode " + os.Args[1])
	}
}
GO

cat > "$SCRATCH/helpers/assemble.go" <<'GO'
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

type OpSummary struct {
	Op      string  `json:"op"`
	Count   int     `json:"count"`
	TotalNS int64   `json:"total_ns"`
	P50NS   int64   `json:"p50_ns"`
	P95NS   int64   `json:"p95_ns"`
	P99NS   int64   `json:"p99_ns"`
	MeanNS  float64 `json:"mean_ns"`
}

type MetaOut struct {
	Kind        string    `json:"kind"`
	Dirs        int       `json:"dirs"`
	FilesPerDir int       `json:"files_per_dir"`
	Create      OpSummary `json:"create"`
	Stat        OpSummary `json:"stat"`
	Readdir     OpSummary `json:"readdir"`
	Unlink      OpSummary `json:"unlink"`
}

type OverOut struct {
	Kind       string    `json:"kind"`
	FileSize   int64     `json:"file_size"`
	Count      int       `json:"count"`
	BlockBytes int       `json:"block_bytes"`
	Total      OpSummary `json:"total"`
	Sync       OpSummary `json:"sync"`
}

type ReaddirOut struct {
	Kind    string `json:"kind"`
	Entries int    `json:"entries"`
	NS      int64  `json:"ns"`
}

type fioPct struct {
	Percentile map[string]float64 `json:"percentile"`
}

type fioSection struct {
	Iops   float64 `json:"iops"`
	BW     float64 `json:"bw"`
	ClatNS fioPct  `json:"clat_ns"`
	LatNS  fioPct  `json:"lat_ns"`
}

type fioRoot struct {
	Jobs []struct {
		Write fioSection `json:"write"`
		Read  fioSection `json:"read"`
	} `json:"jobs"`
}

func median(xs []float64) float64 {
	if len(xs) == 0 {
		return 0
	}
	s := append([]float64(nil), xs...)
	sort.Float64s(s)
	mid := len(s) / 2
	if len(s)%2 == 0 {
		return (s[mid-1] + s[mid]) / 2
	}
	return s[mid]
}

func pickPct(m map[string]float64, want float64) float64 {
	best := 0.0
	bestDelta := 1e18
	for label, value := range m {
		parsed, err := strconv.ParseFloat(label, 64)
		if err != nil {
			continue
		}
		delta := parsed - want
		if delta < 0 {
			delta = -delta
		}
		if delta < bestDelta {
			bestDelta = delta
			best = value
		}
	}
	return best
}

func readJSON(path string, v interface{}) error {
	b, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	return json.Unmarshal(b, v)
}

func parseKV(path string) map[string]string {
	out := map[string]string{}
	b, err := os.ReadFile(path)
	if err != nil {
		return out
	}
	for _, line := range strings.Split(string(b), "\n") {
		if k, v, ok := strings.Cut(line, "="); ok {
			out[strings.TrimSpace(k)] = strings.TrimSpace(v)
		}
	}
	return out
}

func mustRead(path string) string {
	b, _ := os.ReadFile(path)
	return string(b)
}

func medianOp(vals []OpSummary) OpSummary {
	out := OpSummary{Op: "median-across-reps"}
	n := len(vals)
	if n == 0 {
		return out
	}
	p50 := make([]float64, 0, n)
	p95 := make([]float64, 0, n)
	p99 := make([]float64, 0, n)
	mean := make([]float64, 0, n)
	for _, v := range vals {
		p50 = append(p50, float64(v.P50NS))
		p95 = append(p95, float64(v.P95NS))
		p99 = append(p99, float64(v.P99NS))
		mean = append(mean, v.MeanNS)
	}
	out.Count = vals[0].Count
	out.P50NS = int64(median(p50))
	out.P95NS = int64(median(p95))
	out.P99NS = int64(median(p99))
	out.MeanNS = median(mean)
	return out
}

func fileList(dir, prefix, suffix string) []string {
	entries, _ := os.ReadDir(dir)
	var out []string
	for _, e := range entries {
		name := e.Name()
		if strings.HasPrefix(name, prefix) && strings.HasSuffix(name, suffix) {
			out = append(out, name)
		}
	}
	sort.Strings(out)
	return out
}

func main() {
	raw := os.Args[1]
	out := os.Args[2]

	env := parseKV(filepath.Join(raw, "environment.txt"))
	status := parseKV(filepath.Join(raw, "status.txt"))
	goStatus := status["go_status"]
	if goStatus == "" {
		goStatus = "unknown"
	}
	goError := mustRead(filepath.Join(raw, "go-error.txt"))

	// ---- cell 1: metadata ------------------------------------------------
	type targetMeta struct {
		Reps   []map[string]interface{} `json:"reps"`
		Median map[string]interface{}   `json:"median"`
	}
	metaTargets := map[string]*targetMeta{}
	for _, name := range []string{"rust", "go", "ext4", "tmpfs"} {
		metaTargets[name] = &targetMeta{}
	}
	metaRaw := map[string][]MetaOut{}
	metaNames := fileList(raw, "meta-", ".json")
	for _, name := range metaNames {
		// meta-<target>-r<rep>.json
		base := strings.TrimSuffix(strings.TrimPrefix(name, "meta-"), ".json")
		parts := strings.Split(base, "-r")
		if len(parts) != 2 {
			continue
		}
		target := parts[0]
		var m MetaOut
		if err := readJSON(filepath.Join(raw, name), &m); err != nil {
			continue
		}
		rep, _ := strconv.Atoi(parts[1])
		metaRaw[target] = append(metaRaw[target], m)
		rec := map[string]interface{}{"rep": rep, "raw": m}
		if t, ok := metaTargets[target]; ok {
			t.Reps = append(t.Reps, rec)
		}
	}
	for target, m := range metaTargets {
		vals := metaRaw[target]
		if len(vals) == 0 {
			continue
		}
		ops := func(total int64, count int) float64 {
			if total <= 0 {
				return 0
			}
			return float64(count) * 1e9 / float64(total)
		}
		createOps := make([]float64, 0, len(vals))
		statOps := make([]float64, 0, len(vals))
		readdirOps := make([]float64, 0, len(vals))
		unlinkOps := make([]float64, 0, len(vals))
		creates := make([]OpSummary, 0, len(vals))
		stats := make([]OpSummary, 0, len(vals))
		readdirs := make([]OpSummary, 0, len(vals))
		unlinks := make([]OpSummary, 0, len(vals))
		for _, v := range vals {
			createOps = append(createOps, ops(v.Create.TotalNS, v.Create.Count))
			statOps = append(statOps, ops(v.Stat.TotalNS, v.Stat.Count))
			readdirOps = append(readdirOps, ops(v.Readdir.TotalNS, v.Readdir.Count))
			unlinkOps = append(unlinkOps, ops(v.Unlink.TotalNS, v.Unlink.Count))
			creates = append(creates, v.Create)
			stats = append(stats, v.Stat)
			readdirs = append(readdirs, v.Readdir)
			unlinks = append(unlinks, v.Unlink)
		}
		m.Median = map[string]interface{}{
			"ops_per_s": map[string]float64{
				"create":  median(createOps),
				"stat":    median(statOps),
				"readdir": median(readdirOps),
				"unlink":  median(unlinkOps),
			},
			"latency": map[string]OpSummary{
				"create":  medianOp(creates),
				"stat":    medianOp(stats),
				"readdir": medianOp(readdirs),
				"unlink":  medianOp(unlinks),
			},
		}
	}

	// ---- cell 2: fio -----------------------------------------------------
	type fioCell struct {
		Reps   []map[string]interface{} `json:"reps"`
		Median map[string]interface{}   `json:"median"`
	}
	fioTargets := map[string]map[string]*fioCell{}
	fioRaw := map[string]map[string][]fioRoot{}
	for _, name := range fileList(raw, "fio-", ".json") {
		// fio-<target>-<workload>-r<rep>.json
		base := strings.TrimSuffix(strings.TrimPrefix(name, "fio-"), ".json")
		idx := strings.LastIndex(base, "-r")
		if idx < 0 {
			continue
		}
		head, repStr := base[:idx], base[idx+2:]
		target, workload := "", ""
		for _, w := range []string{"randwrite-4k", "seqwrite-1m"} {
			if strings.HasSuffix(head, "-"+w) {
				target = strings.TrimSuffix(head, "-"+w)
				workload = w
				break
			}
		}
		if target == "" || workload == "" {
			continue
		}
		var root fioRoot
		if err := readJSON(filepath.Join(raw, name), &root); err != nil || len(root.Jobs) == 0 {
			continue
		}
		rep, _ := strconv.Atoi(repStr)
		if fioTargets[target] == nil {
			fioTargets[target] = map[string]*fioCell{}
		}
		if fioTargets[target][workload] == nil {
			fioTargets[target][workload] = &fioCell{}
		}
		fioRaw[target] = appendFio(fioRaw[target], workload, root)
		sec := root.Jobs[0].Write
		rec := map[string]interface{}{
			"rep":      rep,
			"raw":      root,
			"iops":     sec.Iops,
			"bw_kib_s": sec.BW,
			"mb_s":     sec.BW / 1024.0,
			"p50_ns":   pickPct(sec.ClatNS.Percentile, 50),
			"p95_ns":   pickPct(sec.ClatNS.Percentile, 95),
			"p99_ns":   pickPct(sec.ClatNS.Percentile, 99),
		}
		fioTargets[target][workload].Reps = append(fioTargets[target][workload].Reps, rec)
	}
	for target, workloads := range fioTargets {
		for workload, cell := range workloads {
			var iops, mb, p50, p95, p99 []float64
			for _, r := range cell.Reps {
				iops = append(iops, r["iops"].(float64))
				mb = append(mb, r["mb_s"].(float64))
				p50 = append(p50, r["p50_ns"].(float64))
				p95 = append(p95, r["p95_ns"].(float64))
				p99 = append(p99, r["p99_ns"].(float64))
			}
			cell.Median = map[string]interface{}{
				"iops": median(iops), "mb_s": median(mb),
				"p50_ns": int64(median(p50)), "p95_ns": int64(median(p95)), "p99_ns": int64(median(p99)),
			}
			_ = target
			_ = workload
		}
	}

	// ---- cell 3: small overwrite ----------------------------------------
	overTargets := map[string]*fioCell{}
	overRaw := map[string][]OverOut{}
	for _, name := range fileList(raw, "overwrite-", ".json") {
		// overwrite-<impl>-r<rep>.json
		base := strings.TrimSuffix(strings.TrimPrefix(name, "overwrite-"), ".json")
		parts := strings.Split(base, "-r")
		if len(parts) != 2 {
			continue
		}
		impl := parts[0]
		var o OverOut
		if err := readJSON(filepath.Join(raw, name), &o); err != nil {
			continue
		}
		rep, _ := strconv.Atoi(parts[1])
		overRaw[impl] = append(overRaw[impl], o)
		if overTargets[impl] == nil {
			overTargets[impl] = &fioCell{}
		}
		overTargets[impl].Reps = append(overTargets[impl].Reps, map[string]interface{}{
			"rep": rep, "raw": o,
			"ops_per_s": func() float64 {
				if o.Total.TotalNS <= 0 {
					return 0
				}
				return float64(o.Count) * 1e9 / float64(o.Total.TotalNS)
			}(),
		})
	}
	for impl, cell := range overTargets {
		vals := overRaw[impl]
		totals := make([]OpSummary, 0, len(vals))
		syncs := make([]OpSummary, 0, len(vals))
		ops := make([]float64, 0, len(vals))
		for _, v := range vals {
			totals = append(totals, v.Total)
			syncs = append(syncs, v.Sync)
			if v.Total.TotalNS > 0 {
				ops = append(ops, float64(v.Count)*1e9/float64(v.Total.TotalNS))
			}
		}
		cell.Median = map[string]interface{}{
			"ops_per_s": median(ops),
			"total":     medianOp(totals),
			"sync":      medianOp(syncs),
		}
	}

	// ---- cell 4: cold mount + context readdir ---------------------------
	type mountRep struct {
		Rep       int     `json:"rep"`
		MountMS   float64 `json:"mount_ms"`
		Entries   int     `json:"entries"`
		ReaddirNS int64   `json:"readdir_ns"`
	}
	var mountReps []mountRep
	for _, name := range fileList(raw, "mount-", ".ms") {
		repStr := strings.TrimSuffix(strings.TrimPrefix(name, "mount-r"), ".ms")
		rep, _ := strconv.Atoi(repStr)
		msText := strings.TrimSpace(mustRead(filepath.Join(raw, name)))
		ms, _ := strconv.ParseFloat(msText, 64)
		var rd ReaddirOut
		rdName := fmt.Sprintf("ctxread-r%d.json", rep)
		if err := readJSON(filepath.Join(raw, rdName), &rd); err != nil {
			continue
		}
		mountReps = append(mountReps, mountRep{Rep: rep, MountMS: ms, Entries: rd.Entries, ReaddirNS: rd.NS})
	}
	sort.Slice(mountReps, func(i, j int) bool { return mountReps[i].Rep < mountReps[j].Rep })
	var mountMedian, mountMax map[string]interface{}
	if len(mountReps) > 0 {
		ms := make([]float64, 0, len(mountReps))
		rd := make([]float64, 0, len(mountReps))
		for _, r := range mountReps {
			ms = append(ms, r.MountMS)
			rd = append(rd, float64(r.ReaddirNS))
		}
		mountMedian = map[string]interface{}{
			"mount_ms": median(ms), "readdir_ms": median(rd) / 1e6, "entries": mountReps[0].Entries,
		}
		mountMax = map[string]interface{}{
			"mount_ms": func() float64 { sort.Float64s(ms); return ms[len(ms)-1] }(),
			"readdir_ms": func() float64 {
				sort.Float64s(rd)
				return rd[len(rd)-1] / 1e6
			}(),
		}
	}

	commands := mustRead(filepath.Join(raw, "commands.txt"))
	fioVersion := strings.TrimSpace(mustRead(filepath.Join(raw, "fio-version.txt")))
	mountInfo := map[string]string{}
	for _, kind := range []string{"rust", "go", "ext4", "tmpfs", "context"} {
		mountInfo[kind] = mustRead(filepath.Join(raw, "mountinfo-"+kind+".txt"))
	}

	report := map[string]interface{}{
		"schema":       "weft-fuse-bench-v1",
		"kind":         "mounted-filesystem-end-to-end",
		"generated_at": timeStamp(),
		"label":        "single host, synthetic deterministic fixtures, buffered I/O; page caches not dropped; observations are not hardware-independent claims",
		"environment":  env,
		"fio_version":  fioVersion,
		"go_status":    goStatus,
		"go_error":     goError,
		"mounts":       mountInfo,
		"commands":     commands,
		"cells": map[string]interface{}{
			"1_metadata": map[string]interface{}{
				"params": map[string]interface{}{
					"dirs": META_DIRS(), "files_per_dir": META_PER(), "files": META_DIRS() * META_PER(),
				},
				"targets": metaTargets,
			},
			"2_fio": map[string]interface{}{
				"params": map[string]interface{}{
					"size": FIO_SIZE(), "randwrite_io_size": RAND_IO(), "seqwrite_io_size": SEQ_IO(),
					"seqwrite_bs": "1M", "randwrite_bs": "4k", "numjobs": 1, "iodepth": 1,
					"ioengine": "psync", "direct": 0,
				},
				"targets": fioTargets,
			},
			"3_small_overwrite": map[string]interface{}{
				"params": map[string]interface{}{
					"file_size": OVER_BYTES(), "count": OVER_COUNT(), "block_bytes": 4096,
					"sync_per_op": true, "targets": "WeftFS (Rust and Go) only",
				},
				"targets": overTargets,
			},
			"4_cold_mount": map[string]interface{}{
				"params": map[string]interface{}{
					"objects": 10000, "context": "saved Context Root over all 10,000 objects",
					"reps": len(mountReps),
				},
				"reps":   mountReps,
				"median": mountMedian,
				"max":    mountMax,
			},
		},
		"page_cache": map[string]interface{}{
			"dropped": false,
			"reason":  "/proc/sys/vm/drop_caches is not writable in this court; no host cache is dropped. See bench/RESULTS-fuse-v0.5.md.",
			"state": map[string]string{
				"cell_1": "entries are created and removed inside the timed loop; first repetition is cold for the mount, later repetitions reuse warm metadata caches",
				"cell_2": "fio is buffered (direct=0); file data may sit in the page cache; end-of-job flush is included in runtime",
				"cell_3": "the 128 MiB file is created with ftruncate (sparse) before the timed overwrites; each overwrite allocates its touched chunks",
				"cell_4": "FUSE caches are empty after each unmount; the backing store files are warm in the host page cache",
			},
		},
		"limitations": []string{
			"tmpfs is a REFERENCE ONLY: no durability, no FUSE layer, not an equivalent filesystem. No superiority claim follows.",
			"ext4 is the host ext4 filesystem behind the /work bind mount. It is not a dedicated block device passed to the container, so host activity and host page cache apply.",
			"fio runs buffered (direct=0). Direct I/O is not benchmarked: same-inode concurrent direct I/O on this kernel/fuser can return EIO (docs/fuse-status.md).",
			"4 KiB random write samples 512 KiB of I/O (128 I/Os) and 1 MiB sequential write samples 64 MiB (64 I/Os) inside a 256 MiB file layout. The Go write path costs O(file size) per write, so full-size sampling exceeds the 30-minute court. IOPS is the real FUSE path; total bytes are bounded and stated.",
			"Cell 3 uses a sparse 128 MiB file created by ftruncate so file preparation does not dominate the court; every timed 4 KiB overwrite still goes through the real write path and fsync.",
			"Cell 4 measures mount-to-ready as 'mount visible and a root getattr succeeds' and the first readdir of the context root. The backing store files are not cold.",
			"Three repetitions give scoped medians and within-run percentiles, not a confidence interval.",
			"No physical power-loss durability is tested here.",
			"Rust and Go are independent daemons over the same on-disk format; differences include implementation, not just language.",
		},
		"notes": []string{
			"All runs are inside one OOM-limited bench court (memory==swap, pids, cpus, bounded /tmp).",
			"Raw per-repetition observations are embedded; raw fio JSON is embedded per repetition.",
			"No `winner` field is emitted. See bench/RESULTS-fuse-v0.5.md for scoped analysis and losses.",
			"Helper programs (metadata/overwrite driver and receipt assembler) are generated under .docker-cache; product code is unchanged.",
		},
	}
	text, err := json.MarshalIndent(report, "", "  ")
	if err != nil {
		panic(err)
	}
	text = append(text, '\n')
	if err := os.WriteFile(out, text, 0o644); err != nil {
		panic(err)
	}
	fmt.Fprintf(os.Stderr, "wrote %s\n", out)
}

func appendFio(m map[string][]fioRoot, workload string, root fioRoot) map[string][]fioRoot {
	if m == nil {
		m = map[string][]fioRoot{}
	}
	m[workload] = append(m[workload], root)
	return m
}

func timeStamp() string {
	return os.Getenv("WEFT_RECEIPT_STAMP")
}

func envInt(name string, def int) int {
	if v, err := strconv.Atoi(os.Getenv(name)); err == nil {
		return v
	}
	return def
}

func META_DIRS() int { return envInt("WEFT_FUSE_META_DIRS", 200) }
func META_PER() int  { return envInt("WEFT_FUSE_META_PER", 10) }
func FIO_SIZE() string { return envOr("WEFT_FUSE_FIO_SIZE", "256M") }
func RAND_IO() string  { return envOr("WEFT_FUSE_RAND_IO", "512K") }
func SEQ_IO() string   { return envOr("WEFT_FUSE_SEQ_IO", "64M") }
func OVER_BYTES() string {
	return envOr("WEFT_FUSE_OVER_BYTES", "128M")
}
func OVER_COUNT() int { return envInt("WEFT_FUSE_OVER_COUNT", 500) }
func envOr(name, def string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return def
}
GO

# ---- run ------------------------------------------------------------------
exec > >(tee -a "$LOG") 2>&1
PS4='+ '
set -x

log() { printf '%s\n' "$*" >&2; }

cleanup() {
  set +e
  for mnt in "$RUST_MNT" "$GO_MNT" "$CTX_MNT"; do
    if mountpoint -q "$mnt" 2>/dev/null; then
      fusermount3 -u "$mnt" 2>/dev/null || umount -l "$mnt" 2>/dev/null || true
    fi
  done
  for pid in "${MOUNT_PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT

record_cmd() { printf '%s\n' "$*" >> "$RAW/commands.txt"; }

start_mount() {
  local cli="$1" store="$2" mnt="$3" name="$4" extra="${5:-}"
  mkdir -p "$mnt"
  record_cmd "$cli --store $store --fixed-timestamp-ns $TS mount --mountpoint $mnt $extra"
  # shellcheck disable=SC2086
  "$cli" --store "$store" --fixed-timestamp-ns "$TS" mount --mountpoint "$mnt" $extra \
    >"$RUN/mount-$name.log" 2>&1 &
  local pid=$!
  MOUNT_PIDS+=("$pid")
  local tries=0
  while ! { mountpoint -q "$mnt" 2>/dev/null && stat -c '%i' "$mnt" >/dev/null 2>&1; }; do
    if ! kill -0 "$pid" 2>/dev/null; then
      log "$name mount daemon exited before the mount was ready"
      cat "$RUN/mount-$name.log" >&2 || true
      return 1
    fi
    tries=$((tries + 1))
    if [ "$tries" -gt 600 ]; then
      log "$name mount was not ready within 60s"
      cat "$RUN/mount-$name.log" >&2 || true
      return 1
    fi
    sleep 0.05
  done
  return 0
}

mountinfo() {
  local name="$1" dir="$2"
  local out="$RAW/mountinfo-$name.txt"
  {
    echo "target=$name"
    echo "path=$dir"
    echo "stat_fs_type=$(stat -f -c '%T' "$dir" 2>/dev/null || echo unknown)"
    echo "stat_fs_blocks=$(stat -f -c '%b' "$dir" 2>/dev/null || echo unknown)"
    findmnt -T "$dir" -o TARGET,SOURCE,FSTYPE,OPTIONS -n 2>/dev/null || true
    grep " $dir " /proc/self/mountinfo 2>/dev/null || true
  } > "$out"
}

reset_fio_target() {
  local dir="$1"
  rm -f "$dir/seqwrite-1m.dat" "$dir/randwrite-4k.dat"
}

now_ns() { date +%s%N; }

# ---- environment ----------------------------------------------------------
log "fuse bench: image=${WEFT_COURT_IMAGE:-unknown} kernel=$(uname -r) reps=$REPS"
fio --version > "$RAW/fio-version.txt" 2>&1 || true
{
  echo "uname=$(uname -a)"
  echo "nproc=$(nproc)"
  echo "cpu=$(awk -F': ' '/model name/ {print $2; exit}' /proc/cpuinfo)"
  echo "memory=$(awk '/MemTotal/ {print $2" kB"; exit}' /proc/meminfo)"
  echo "court_image=${WEFT_COURT_IMAGE:-unknown}"
  echo "court_image_id=${WEFT_COURT_IMAGE_ID:-unknown}"
  echo "court_kernel=${WEFT_COURT_KERNEL:-unknown}"
  echo "court_memory=${WEFT_COURT_MEMORY:-unknown}"
  echo "court_cpus=${WEFT_COURT_CPUS:-unknown}"
  echo "court_pids=${WEFT_COURT_PIDS:-unknown}"
  echo "docker_client=${WEFT_DOCKER_CLIENT:-unknown}"
  echo "docker_server=${WEFT_DOCKER_SERVER:-unknown}"
  echo "revision_weftfs=${WEFT_REPO_WEFTFS:-unknown}"
  echo "revision_weftfs_go=${WEFT_REPO_WEFTFS_GO:-unknown}"
  echo "revision_weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
  echo "revision_weftcodec_go=${WEFT_REPO_WEFTCODEC_GO:-unknown}"
  echo "page_cache_dropped=false"
  echo "fio_direct=0"
  echo "reps=$REPS"
  echo "fio_size=$FIO_SIZE"
  echo "randwrite_io_size=$RAND_IO"
  echo "seqwrite_io_size=$SEQ_IO"
  echo "overwrite_bytes=$OVER_BYTES"
  echo "overwrite_count=$OVER_COUNT"
} > "$RAW/environment.txt"

# ---- build ----------------------------------------------------------------
record_cmd "cargo build --release --features fuse --bin weftfs"
cargo build --release --features fuse --bin weftfs
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}/release"
RUSTFS="$TARGET/weftfs"
[ -x "$RUSTFS" ]

record_cmd "(cd WeftFS-Go && CGO_ENABLED=0 go build -o $BIN/go-weftfs ./cmd/weftfs)"
if (cd "$ROOT/WeftFS-Go" && CGO_ENABLED=0 go build -o "$BIN/go-weftfs" ./cmd/weftfs) \
  >>"$RUN/go-build.log" 2>&1; then
  GO_STATUS="ok"
else
  GO_STATUS="blocked"
  { echo "go build failed"; cat "$RUN/go-build.log"; } > "$RAW/go-error.txt"
fi
GOFS="$BIN/go-weftfs"

record_cmd "(cd .docker-cache/fuse-bench/helpers && CGO_ENABLED=0 go build -o $BIN/metaops metaops.go)"
(cd "$SCRATCH/helpers" && CGO_ENABLED=0 go build -o "$BIN/metaops" metaops.go)
record_cmd "(cd .docker-cache/fuse-bench/helpers && CGO_ENABLED=0 go build -o $BIN/assemble assemble.go)"
(cd "$SCRATCH/helpers" && CGO_ENABLED=0 go build -o "$BIN/assemble" assemble.go)
if [ "${WEFT_FUSE_HELPERS_ONLY:-0}" = "1" ]; then
  echo "helpers built: $BIN/metaops $BIN/assemble"
  exit 0
fi

# ---- mounts ---------------------------------------------------------------
record_cmd "$RUSTFS --store $RUST_STORE --fixed-timestamp-ns $TS init"
"$RUSTFS" --store "$RUST_STORE" --fixed-timestamp-ns "$TS" init
if ! start_mount "$RUSTFS" "$RUST_STORE" "$RUST_MNT" rust --foreground; then
  log "Rust WeftFS mount failed"
  exit 1
fi
mountinfo rust "$RUST_MNT"

GO_MNT_ACTIVE=0
if [ "$GO_STATUS" = "ok" ]; then
  if "$GOFS" --store "$GO_STORE" --fixed-timestamp-ns "$TS" init \
      >"$RUN/go-init.log" 2>&1; then
    if start_mount "$GOFS" "$GO_STORE" "$GO_MNT" go; then
      GO_MNT_ACTIVE=1
      mountinfo go "$GO_MNT"
    else
      GO_STATUS="blocked"
      { echo "go mount failed"; cat "$RUN/mount-go.log"; } > "$RAW/go-error.txt"
    fi
  else
    GO_STATUS="blocked"
    { echo "go init failed"; cat "$RUN/go-init.log"; } > "$RAW/go-error.txt"
  fi
fi
mountinfo ext4 "$EXT4_DIR"
mountinfo tmpfs "$TMPFS_DIR"
printf 'go_status=%s\n' "$GO_STATUS" > "$RAW/status.txt"

# ---- cell 1: metadata -----------------------------------------------------
target_dir() {
  case "$1" in
    rust) echo "$RUST_MNT" ;;
    go) echo "$GO_MNT" ;;
    ext4) echo "$EXT4_DIR" ;;
    tmpfs) echo "$TMPFS_DIR" ;;
  esac
}

for rep in $(seq 1 "$REPS"); do
  if [ $((rep % 2)) -eq 0 ]; then
    order=(tmpfs ext4 rust)
    [ "$GO_MNT_ACTIVE" = "1" ] && order=(tmpfs ext4 go rust)
  else
    order=(rust ext4 tmpfs)
    [ "$GO_MNT_ACTIVE" = "1" ] && order=(rust go ext4 tmpfs)
  fi
  for target in "${order[@]}"; do
    [ "$target" = "go" ] && [ "$GO_MNT_ACTIVE" != "1" ] && continue
    dir="$(target_dir "$target")"
    root="$dir/meta-r$rep"
    record_cmd "$BIN/metaops meta $root $META_DIRS $META_PER"
    "$BIN/metaops" meta "$root" "$META_DIRS" "$META_PER" > "$RAW/meta-$target-r$rep.json"
    rmdir "$root" 2>/dev/null || true
  done
  log "fuse bench: metadata repetition $rep/$REPS complete"
done

# ---- cell 2: fio 4k randwrite and 1M seqwrite ----------------------------
run_fio() {
  local target="$1" dir="$2" workload="$3" raw="$4"
  local rw bs size io_size
  case "$workload" in
    randwrite-4k) rw=randwrite; bs=4k; size="$FIO_SIZE"; io_size="--io_size=$RAND_IO" ;;
    seqwrite-1m) rw=write; bs=1M; size="$FIO_SIZE"; io_size="--io_size=$SEQ_IO" ;;
    *) log "unknown workload $workload"; return 1 ;;
  esac
  record_cmd "fio --name=$workload --directory=$dir --filename=${workload}.dat --rw=$rw --bs=$bs --size=$size $io_size --randseed=1234 --ioengine=psync --direct=0 --numjobs=1 --iodepth=1 --group_reporting --output-format=json --output=$raw --eta=never --percentile_list=50:95:99"
  # shellcheck disable=SC2086
  fio --name="$workload" --directory="$dir" --filename="${workload}.dat" \
    --rw="$rw" --bs="$bs" --size="$size" $io_size --randseed=1234 \
    --ioengine=psync --direct=0 --numjobs=1 --iodepth=1 --group_reporting \
    --output-format=json --output="$raw" --eta=never --percentile_list=50:95:99 \
    >/dev/null 2>>"$LOG"
  [ -s "$raw" ]
}

for rep in $(seq 1 "$REPS"); do
  if [ $((rep % 2)) -eq 0 ]; then
    order=(tmpfs ext4 rust)
    [ "$GO_MNT_ACTIVE" = "1" ] && order=(tmpfs ext4 go rust)
  else
    order=(rust ext4 tmpfs)
    [ "$GO_MNT_ACTIVE" = "1" ] && order=(rust go ext4 tmpfs)
  fi
  for target in "${order[@]}"; do
    [ "$target" = "go" ] && [ "$GO_MNT_ACTIVE" != "1" ] && continue
    dir="$(target_dir "$target")"
    reset_fio_target "$dir"
    for workload in randwrite-4k seqwrite-1m; do
      run_fio "$target" "$dir" "$workload" "$RAW/fio-$target-$workload-r$rep.json"
    done
    reset_fio_target "$dir"
  done
  log "fuse bench: fio repetition $rep/$REPS complete"
done

# ---- cell 3: 500 random 4 KiB overwrites into a 128 MiB file -------------
for rep in $(seq 1 "$REPS"); do
  order=(rust)
  [ "$GO_MNT_ACTIVE" = "1" ] && order=(rust go)
  [ $((rep % 2)) -eq 0 ] && [ "$GO_MNT_ACTIVE" = "1" ] && order=(go rust)
  for impl in "${order[@]}"; do
    dir="$(target_dir "$impl")"
    file="$dir/overwrite-$rep.dat"
    record_cmd "truncate -s 128M $file"
    truncate -s 128M "$file"
    record_cmd "$BIN/metaops overwrite $file $OVER_COUNT 4096"
    "$BIN/metaops" overwrite "$file" "$OVER_COUNT" 4096 > "$RAW/overwrite-$impl-r$rep.json"
    rm -f "$file"
  done
  log "fuse bench: overwrite repetition $rep/$REPS complete"
done

# ---- cell 4: cold mount of a 10,000-object store + context ls ------------
mkdir -p "$SCRATCH/builder/src"
cat > "$SCRATCH/builder/Cargo.toml" <<'TOML'
[package]
name = "weft-fuse-bench-builder"
version = "0.0.0"
edition = "2021"

[workspace]

[dependencies]
weftfs = { path = "/work" }
weftcodec = { path = "/work/WeftCodec" }

[profile.release]
debug = false
TOML
cat > "$SCRATCH/builder/src/main.rs" <<'RS'
use std::collections::BTreeMap;
use std::env;
use weftcodec::wcb::Value as WcbValue;
use weftfs::store::{Options, Store};

fn main() {
    let mut path = None;
    let mut count = 10_000usize;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--store" => path = Some(args.next().expect("store path")),
            "--count" => count = args.next().expect("count").parse().expect("count"),
            other => panic!("unknown argument {other}"),
        }
    }
    let path = path.expect("--store is required");
    let options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 65_536,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_767_225_600_000_000_000),
        ..Options::default()
    };
    let mut store = Store::init(&path, options).expect("init store");
    let root = store.resolve_path("/").expect("root");
    let mut attrs = BTreeMap::new();
    attrs.insert("bench".to_owned(), WcbValue::Str("fuse-v05".to_owned()));
    let ((), head) = store
        .group(|s| {
            for index in 0..count {
                let name = format!("obj-{index:06}.dat");
                let seed = format!("fuse-bench-{count}-{index}");
                let bytes = format!("weft fuse bench object {index:06}\n");
                s.create_file_seeded(
                    root,
                    name.as_bytes(),
                    bytes.as_bytes(),
                    0o644,
                    0,
                    0,
                    attrs.clone(),
                    seed.as_bytes(),
                )?;
            }
            Ok(())
        })
        .expect("group create");
    println!("created={count} head={}", head.to_hex());
}
RS
record_cmd "cargo build --offline --release --manifest-path $SCRATCH/builder/Cargo.toml"
BUILD_TARGET="$ROOT/.docker-cache/target-rust"
CARGO_TARGET_DIR="$BUILD_TARGET" cargo build --offline --release \
  --manifest-path "$SCRATCH/builder/Cargo.toml"
BUILDER="$BUILD_TARGET/release/weft-fuse-bench-builder"
[ -x "$BUILDER" ]
record_cmd "$BUILDER --store $CTX_STORE --count 10000"
CTX_BUILD_START=$(now_ns)
"$BUILDER" --store "$CTX_STORE" --count 10000 | tee "$RAW/context-build.txt"
CTX_BUILD_END=$(now_ns)
record_cmd "$RUSTFS --store $CTX_STORE snapshot create all"
"$RUSTFS" --store "$CTX_STORE" snapshot create all >> "$RAW/context-build.txt"
record_cmd "$RUSTFS --store $CTX_STORE context create --name all --selector 'select attrs.bench == \"fuse-v05\"' --snapshot all"
"$RUSTFS" --store "$CTX_STORE" context create --name all \
  --selector 'select attrs.bench == "fuse-v05"' --snapshot all >> "$RAW/context-build.txt"
CTX_SETUP_END=$(now_ns)
printf 'store_build_ms=%s\ncontext_setup_ms=%s\n' \
  $(( (CTX_BUILD_END - CTX_BUILD_START) / 1000000 )) \
  $(( (CTX_SETUP_END - CTX_BUILD_END) / 1000000 )) >> "$RAW/context-build.txt"

for rep in $(seq 1 "$REPS"); do
  mnt="$CTX_MNT"
  mkdir -p "$mnt"
  record_cmd "$RUSTFS --store $CTX_STORE mount --mountpoint $mnt --foreground --context all"
  start=$(now_ns)
  "$RUSTFS" --store "$CTX_STORE" --fixed-timestamp-ns "$TS" mount --mountpoint "$mnt" \
    --foreground --context all >"$RUN/mount-context-r$rep.log" 2>&1 &
  pid=$!
  MOUNT_PIDS+=("$pid")
  ready=0
  for i in $(seq 1 1200); do
    if mountpoint -q "$mnt" 2>/dev/null && stat -c '%i' "$mnt" >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 0.005
  done
  end=$(now_ns)
  if [ "$ready" != "1" ]; then
    log "context mount was not ready"
    cat "$RUN/mount-context-r$rep.log" >&2 || true
    exit 1
  fi
  printf '%s\n' "$(awk -v a="$start" -v b="$end" 'BEGIN {printf "%.3f", (b-a)/1000000}')" > "$RAW/mount-r$rep.ms"
  if [ "$rep" = "1" ]; then mountinfo context "$mnt"; fi
  "$BIN/metaops" readdir-one "$mnt" > "$RAW/ctxread-r$rep.json"
  fusermount3 -u "$mnt" 2>/dev/null || true
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  sleep 0.2
  log "fuse bench: context mount repetition $rep/$REPS complete"
done

# ---- receipt --------------------------------------------------------------
export WEFT_RECEIPT_STAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
for var in WEFT_FUSE_META_DIRS WEFT_FUSE_META_PER WEFT_FUSE_FIO_SIZE \
           WEFT_FUSE_RAND_IO WEFT_FUSE_SEQ_IO WEFT_FUSE_OVER_BYTES WEFT_FUSE_OVER_COUNT; do
  export "$var=${!var:-}"
done
"$BIN/assemble" "$RAW" "$RECEIPT"

echo "fuse receipt: $RECEIPT"
echo "raw log: $LOG"
sha256sum "$RECEIPT" "$LOG"
