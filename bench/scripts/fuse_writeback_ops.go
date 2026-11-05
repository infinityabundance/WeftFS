// Command wbops drives POSIX workloads for the FUSE write-back benchmark.
//
// It is built inside the court from this source and never linked into the
// product. Every mode prints one JSON object on stdout.
//
//	wbops overwrite <path> <count> <block-bytes> <fsync:end|each|none> <seed>
//	wbops meta      <root> <dirs> <files-per-dir>
//	wbops crashwrite <path> <old|buffered|durable>
//	wbops crashmeta  <dir> <files>
//
// crashwrite/crashmeta finish their writes and then sleep for ten minutes so
// the harness can SIGKILL the mount daemon while the file/namespace state is
// known.
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

func summarize(ns []int64) map[string]interface{} {
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
	ops := 0.0
	if total > 0 {
		ops = float64(len(s)) * 1e9 / float64(total)
	}
	return map[string]interface{}{
		"count": len(s), "total_ns": total, "ops_per_s": ops,
		"p50_ns": pct(s, 0.50), "p95_ns": pct(s, 0.95),
		"p99_ns": pct(s, 0.99), "mean_ns": mean,
	}
}

func parseU64(text string) uint64 {
	var value uint64
	if _, err := fmt.Sscanf(text, "%d", &value); err != nil {
		panic("invalid integer " + text)
	}
	return value
}

func overwrite(args []string) {
	path := args[0]
	count := int(parseU64(args[1]))
	block := int(parseU64(args[2]))
	syncMode := args[3]
	state := parseU64(args[4])
	if block <= 0 {
		panic("block must be positive")
	}
	info, err := os.Stat(path)
	if err != nil {
		panic(err)
	}
	size := info.Size()
	file, err := os.OpenFile(path, os.O_RDWR, 0)
	if err != nil {
		panic(err)
	}
	defer file.Close()
	buf := make([]byte, block)
	for i := range buf {
		buf[i] = byte(i*31 + 7)
	}
	rand := func() uint64 {
		state ^= state << 13
		state ^= state >> 7
		state ^= state << 17
		return state
	}
	max := size - int64(block)
	if max < 0 {
		max = 0
	}
	writes := make([]int64, 0, count)
	syncs := make([]int64, 0, count)
	totalStart := time.Now()
	for i := 0; i < count; i++ {
		off := int64(rand() % uint64(max+1))
		t0 := time.Now()
		if _, err := file.WriteAt(buf, off); err != nil {
			panic(fmt.Sprintf("write %d: %v", i, err))
		}
		writes = append(writes, time.Since(t0).Nanoseconds())
		if syncMode == "each" {
			t1 := time.Now()
			if err := file.Sync(); err != nil {
				panic(fmt.Sprintf("fsync %d: %v", i, err))
			}
			syncs = append(syncs, time.Since(t1).Nanoseconds())
		}
	}
	finalSync := int64(0)
	if syncMode == "end" {
		t0 := time.Now()
		if err := file.Sync(); err != nil {
			panic(fmt.Sprintf("final fsync: %v", err))
		}
		finalSync = time.Since(t0).Nanoseconds()
	}
	out := map[string]interface{}{
		"kind": "overwrite", "path": path, "file_size": size,
		"count": count, "block_bytes": block, "fsync": syncMode,
		"writes": summarize(writes), "final_fsync_ns": finalSync,
		"wall_ns": time.Since(totalStart).Nanoseconds(),
	}
	emit(out)
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
		path := filepath.Join(root, fmt.Sprintf("d%03d", i))
		if err := os.Mkdir(path, 0o755); err != nil {
			panic(err)
		}
		dirPaths[i] = path
	}
	create := make([]int64, 0, dirs*per)
	stat := make([]int64, 0, dirs*per)
	unlink := make([]int64, 0, dirs*per)
	readdir := make([]int64, 0, dirs)
	files := make([]string, 0, dirs*per)
	for _, dir := range dirPaths {
		for j := 0; j < per; j++ {
			path := filepath.Join(dir, fmt.Sprintf("f%04d", j))
			t0 := time.Now()
			f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o644)
			if err != nil {
				panic(err)
			}
			f.Close()
			create = append(create, time.Since(t0).Nanoseconds())
			files = append(files, path)
		}
	}
	for _, path := range files {
		t0 := time.Now()
		if _, err := os.Lstat(path); err != nil {
			panic(err)
		}
		stat = append(stat, time.Since(t0).Nanoseconds())
	}
	for _, dir := range dirPaths {
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
		if len(names) != per {
			panic(fmt.Sprintf("readdir %s: got %d want %d", dir, len(names), per))
		}
		readdir = append(readdir, time.Since(t0).Nanoseconds())
	}
	for _, path := range files {
		t0 := time.Now()
		if err := os.Remove(path); err != nil {
			panic(err)
		}
		unlink = append(unlink, time.Since(t0).Nanoseconds())
	}
	for _, dir := range dirPaths {
		if err := os.Remove(dir); err != nil {
			panic(err)
		}
	}
	emit(map[string]interface{}{
		"kind": "meta", "root": root, "dirs": dirs, "files_per_dir": per,
		"create": summarize(create), "stat": summarize(stat),
		"readdir": summarize(readdir), "unlink": summarize(unlink),
	})
}

func crashwrite(args []string) {
	path := args[0]
	mode := args[1]
	const size = 64 * 1024
	file, err := os.OpenFile(path, os.O_CREATE|os.O_TRUNC|os.O_RDWR, 0o644)
	if err != nil {
		panic(err)
	}
	old := make([]byte, size)
	for i := range old {
		old[i] = 0x11
	}
	if _, err := file.WriteAt(old, 0); err != nil {
		panic(err)
	}
	if err := file.Sync(); err != nil {
		panic(err)
	}
	emit(map[string]interface{}{"kind": "crashwrite", "phase": mode, "stage": "old-durable"})
	if mode == "old" {
		sleep()
		return
	}
	fresh := make([]byte, size)
	for i := range fresh {
		fresh[i] = 0x22
	}
	if _, err := file.WriteAt(fresh, 0); err != nil {
		panic(err)
	}
	if mode == "durable" {
		if err := file.Sync(); err != nil {
			panic(err)
		}
	}
	emit(map[string]interface{}{"kind": "crashwrite", "phase": mode, "stage": "new-sent"})
	sleep()
}

func crashmeta(args []string) {
	dir := args[0]
	count := int(parseU64(args[1]))
	for i := 0; i < count; i++ {
		path := filepath.Join(dir, fmt.Sprintf("c-%03d.txt", i))
		f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o644)
		if err != nil {
			panic(err)
		}
		if _, err := f.Write([]byte("meta")); err != nil {
			panic(err)
		}
		f.Close()
	}
	emit(map[string]interface{}{"kind": "crashmeta", "files": count})
	sleep()
}

func sleep() {
	time.Sleep(10 * time.Minute)
}

func emit(value interface{}) {
	encoded, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	fmt.Println(string(encoded))
}

func main() {
	if len(os.Args) < 2 {
		panic("usage: wbops <overwrite|meta|crashwrite|crashmeta> ...")
	}
	switch os.Args[1] {
	case "overwrite":
		overwrite(os.Args[2:])
	case "meta":
		meta(os.Args[2:])
	case "crashwrite":
		crashwrite(os.Args[2:])
	case "crashmeta":
		crashmeta(os.Args[2:])
	default:
		panic("unknown mode " + os.Args[1])
	}
}
