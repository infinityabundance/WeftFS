// Command decoder_bomb_court runs the committed adversarial vectors against
// the independent Rust and Go WeftCodec decoders.
//
// It is deliberately stdlib-only: the court image contains no test framework
// beyond the Go toolchain. For every vector it:
//
//   - writes the vector input, dependency files, and frame source;
//   - invokes both CLIs under `timeout 10s /usr/bin/time`;
//   - asserts nonzero exit, the expected normative class in both, identical
//     classes, no output file, no timeout, and no OOM kill (exit 137);
//   - records maximum RSS and wall time from GNU time;
//   - repeats the vector and requires the classes to be stable.
//
// The receipt is written by this program so no shell quoting can corrupt it.
package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strings"
	"syscall"
	"time"
)

type fileEntry struct {
	Name string `json:"name"`
	Hex  string `json:"hex"`
}

type hashEntry struct {
	Hash string `json:"hash"`
	Hex  string `json:"hex"`
}

type Vector struct {
	Name        string            `json:"name"`
	Description string            `json:"description"`
	Verb        string            `json:"verb"`
	ExpectClass string            `json:"expect_class"`
	InputHex    string            `json:"input_hex"`
	Flags       map[string]string `json:"flags"`
	Deps        []hashEntry       `json:"deps"`
	FrameSource []hashEntry       `json:"frame_source"`
	Files       []fileEntry       `json:"files"`
}

type cliRun struct {
	ExitCode   int     `json:"exit_code"`
	Signal     string  `json:"signal,omitempty"`
	Class      string  `json:"class"`
	StderrTail string  `json:"stderr_tail,omitempty"`
	StdoutTail string  `json:"stdout_tail,omitempty"`
	MaxRSSKB   int64   `json:"max_rss_kb"`
	WallSec    float64 `json:"wall_seconds"`
	TimedOut   bool    `json:"timed_out"`
	OOMKilled  bool    `json:"oom_killed"`
	OutputMade bool    `json:"output_created"`
}

type vectorResult struct {
	Name          string            `json:"name"`
	Description   string            `json:"description,omitempty"`
	Verb          string            `json:"verb"`
	SHA256        string            `json:"sha256"`
	ExpectedClass string            `json:"expected_class"`
	Flags         map[string]string `json:"flags,omitempty"`
	Rust          []cliRun          `json:"rust"`
	Go            []cliRun          `json:"go"`
	ClassesStable bool              `json:"classes_stable"`
	Pass          bool              `json:"pass"`
	Failures      []string          `json:"failures,omitempty"`
}

type summary struct {
	Total           int            `json:"total"`
	Pass            int            `json:"pass"`
	Fail            int            `json:"fail"`
	AllPass         bool           `json:"all_pass"`
	Classes         map[string]int `json:"expected_class_counts"`
	MirrorIdentical bool           `json:"mirror_identical"`
	VectorCountOK   bool           `json:"at_least_20_vectors"`
	MinVectorCount  int            `json:"minimum_required"`
}

type environment struct {
	Schema       string            `json:"schema"`
	GeneratedAt  string            `json:"generated_at"`
	Court        string            `json:"court"`
	Kernel       string            `json:"kernel"`
	Arch         string            `json:"arch"`
	Image        string            `json:"image"`
	ImageID      string            `json:"image_id"`
	DockerServer string            `json:"docker_server"`
	DockerClient string            `json:"docker_client"`
	Memory       string            `json:"limit_memory"`
	MemorySwap   string            `json:"limit_memory_swap"`
	CPUs         string            `json:"limit_cpus"`
	PIDs         string            `json:"limit_pids"`
	TmpfsTmp     string            `json:"tmpfs_tmp"`
	GoVersion    string            `json:"go_version"`
	RustcVersion string            `json:"rustc_version"`
	Repositories map[string]string `json:"repositories"`
	// Explicitly separate from environment in the JSON below.
}

type receipt struct {
	Schema      string         `json:"schema"`
	GeneratedAt string         `json:"generated_at"`
	Environment environment    `json:"environment"`
	Court       map[string]any `json:"court"`
	VectorsDir  string         `json:"vectors_dir"`
	MirrorDir   string         `json:"mirror_dir"`
	TimeoutSecs int            `json:"timeout_seconds"`
	Repeat      int            `json:"repeat"`
	Results     []vectorResult `json:"results"`
	Summary     summary        `json:"summary"`
}

var classPattern = regexp.MustCompile(`(?m)^([A-Z][A-Za-z]+):`)
var knownClasses = map[string]bool{
	"Malformed": true, "Corrupt": true, "UnsupportedVersion": true,
	"UnsupportedMode": true, "UnsupportedFeature": true, "LimitExceeded": true,
	"DependencyMissing": true, "DependencyDenied": true,
	"DependencyMismatch": true, "DependencyCycle": true, "NonCanonical": true,
	"Io": true, "Parse": true, "UnknownSchema": true,
}

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "decoder_bomb_court: "+format+"\n", args...)
	os.Exit(65)
}

func sha256Hex(b []byte) string {
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])
}

func tail(text string, limit int) string {
	if len(text) <= limit {
		return text
	}
	return text[len(text)-limit:]
}

func parseClass(stderr string) string {
	for _, match := range classPattern.FindAllStringSubmatch(stderr, -1) {
		if len(match) == 2 && knownClasses[match[1]] {
			return match[1]
		}
	}
	// Go's flag package prefixes errors with "flag provided but not defined";
	// the CLI wraps parse failures as `Parse: ...` on a later line. Rust clap
	// exits 2 before our error printer, so there may be no class at all.
	return ""
}

type timeInfo struct {
	maxRSSKB int64
	wallSec  float64
}

func parseTimeFile(path string) timeInfo {
	b, err := os.ReadFile(path)
	if err != nil {
		return timeInfo{}
	}
	var info timeInfo
	for _, field := range strings.Fields(string(b)) {
		switch {
		case strings.HasPrefix(field, "maxrss_kb="):
			fmt.Sscanf(strings.TrimPrefix(field, "maxrss_kb="), "%d", &info.maxRSSKB)
		case strings.HasPrefix(field, "wall_s="):
			fmt.Sscanf(strings.TrimPrefix(field, "wall_s="), "%f", &info.wallSec)
		}
	}
	return info
}

func runOne(cli string, args []string, dir string, outPath string, outIsDir bool, timeoutSecs int) cliRun {
	run := cliRun{ExitCode: -999}
	timeFile := filepath.Join(dir, "time.txt")
	timedArgs := []string{
		"-k", "1s", fmt.Sprintf("%ds", timeoutSecs),
		"/usr/bin/time", "-f", "WEFT_TIME maxrss_kb=%M wall_s=%e exit=%x",
		"-o", timeFile, "--", cli,
	}
	timedArgs = append(timedArgs, args...)
	cmd := exec.Command("/usr/bin/timeout", timedArgs...)
	cmd.Dir = dir
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	err := cmd.Run()
	info := parseTimeFile(timeFile)
	run.MaxRSSKB = info.maxRSSKB
	run.WallSec = info.wallSec
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			run.ExitCode = exitErr.ExitCode()
			if status, ok := exitErr.Sys().(syscall.WaitStatus); ok {
				if status.Signaled() {
					run.Signal = status.Signal().String()
					if status.Signal() == syscall.SIGKILL {
						run.OOMKilled = run.ExitCode == 137 || run.ExitCode == -1
					}
				}
			}
		} else {
			run.ExitCode = -998
			run.Signal = err.Error()
		}
	} else {
		run.ExitCode = 0
	}
	if run.ExitCode == 124 || run.ExitCode == 137 {
		run.TimedOut = run.ExitCode == 124
		run.OOMKilled = run.ExitCode == 137
	}
	run.Class = parseClass(stderr.String())
	run.StderrTail = tail(stderr.String(), 4000)
	run.StdoutTail = tail(stdout.String(), 1000)

	if outIsDir {
		entries, statErr := os.ReadDir(outPath)
		run.OutputMade = statErr == nil && len(entries) > 0
	} else {
		if _, statErr := os.Stat(outPath); statErr == nil {
			run.OutputMade = true
		}
	}
	return run
}

func writeFile(path string, hexBytes string) error {
	b, err := hex.DecodeString(hexBytes)
	if err != nil {
		return fmt.Errorf("bad hex for %s: %w", path, err)
	}
	return os.WriteFile(path, b, 0o644)
}

func prepare(vector Vector, root string) (inputPath, depsDir, sourceDir, outPath, outDir string, filePaths []string, err error) {
	inputPath = filepath.Join(root, "input.bin")
	if vector.InputHex != "" {
		if err = writeFile(inputPath, vector.InputHex); err != nil {
			return
		}
	} else if err = os.WriteFile(inputPath, nil, 0o644); err != nil {
		return
	}
	if len(vector.Deps) > 0 {
		depsDir = filepath.Join(root, "deps")
		if err = os.MkdirAll(depsDir, 0o755); err != nil {
			return
		}
		for _, dep := range vector.Deps {
			if err = writeFile(filepath.Join(depsDir, dep.Hash), dep.Hex); err != nil {
				return
			}
		}
	}
	if len(vector.FrameSource) > 0 {
		sourceDir = filepath.Join(root, "frames")
		if err = os.MkdirAll(sourceDir, 0o755); err != nil {
			return
		}
		for _, frame := range vector.FrameSource {
			if err = writeFile(filepath.Join(sourceDir, frame.Hash), frame.Hex); err != nil {
				return
			}
		}
	}
	outPath = filepath.Join(root, "out.bin")
	outDir = filepath.Join(root, "outdir")
	for _, file := range vector.Files {
		path := filepath.Join(root, file.Name)
		if err = writeFile(path, file.Hex); err != nil {
			return
		}
		filePaths = append(filePaths, path)
	}
	return
}

func vectorArgs(vector Vector, inputPath, depsDir, sourceDir, outPath, outDir string, filePaths []string) []string {
	var args []string
	switch vector.Verb {
	case "decode":
		args = []string{"decode", "--input", inputPath, "--output", outPath}
		if depsDir != "" {
			args = append(args, "--deps", depsDir)
		}
		if sourceDir != "" {
			args = append(args, "--frame-source", sourceDir)
		}
	case "archive-create":
		args = []string{"archive", "create", "--output", outPath}
	case "archive-decode":
		args = []string{"archive", "decode", "--input", inputPath, "--out-dir", outDir}
	case "selector-id":
		args = []string{"selector", "id", "--input", inputPath}
	default:
		fail("unknown vector verb %q", vector.Verb)
	}
	// Flags go before positionals so Go's flag package sees them.
	keys := make([]string, 0, len(vector.Flags))
	for key := range vector.Flags {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	var flags []string
	for _, key := range keys {
		flags = append(flags, key+"="+vector.Flags[key])
	}
	args = append(args, flags...)
	if vector.Verb == "archive-create" {
		args = append(args, filePaths...)
	}
	return args
}

func readVectors(dir string) ([]Vector, []string, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, nil, err
	}
	var vectors []Vector
	var names []string
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") || entry.Name() == "PROVENANCE.json" {
			continue
		}
		path := filepath.Join(dir, entry.Name())
		b, err := os.ReadFile(path)
		if err != nil {
			return nil, nil, err
		}
		var vector Vector
		if err := json.Unmarshal(b, &vector); err != nil {
			return nil, nil, fmt.Errorf("%s: %w", path, err)
		}
		if vector.Name == "" || vector.Verb == "" || vector.ExpectClass == "" {
			return nil, nil, fmt.Errorf("%s: incomplete vector", path)
		}
		vectors = append(vectors, vector)
		names = append(names, entry.Name())
	}
	// os.ReadDir returns entries sorted by filename, so vectors and names stay
	// aligned without a second sort.
	return vectors, names, nil
}

func main() {
	rustCLI := flag.String("rust", "", "path to the Rust weftcodec binary")
	goCLI := flag.String("go", "", "path to the Go weftcodec binary")
	vectorsDir := flag.String("vectors", "", "primary adversarial vector directory")
	mirrorDir := flag.String("mirror", "", "byte-identical mirror directory")
	outPath := flag.String("out", "", "receipt JSON path")
	stamp := flag.String("stamp", "", "receipt timestamp")
	timeoutSecs := flag.Int("timeout", 10, "per-CLI timeout in seconds")
	repeat := flag.Int("repeat", 2, "runs per CLI per vector (class stability)")
	maxRSSLimit := flag.Int64("max-rss-kb", 512*1024, "fail if a run exceeds this maximum RSS (KiB)")
	flag.Parse()
	if *rustCLI == "" || *goCLI == "" || *vectorsDir == "" || *mirrorDir == "" || *outPath == "" {
		fail("--rust, --go, --vectors, --mirror, and --out are required")
	}
	if *repeat < 2 {
		fail("--repeat must be at least 2 to prove class stability")
	}

	vectors, fileNames, err := readVectors(*vectorsDir)
	if err != nil {
		fail("read vectors: %v", err)
	}
	if len(vectors) < 20 {
		fail("need at least 20 vectors, found %d", len(vectors))
	}
	mirrorIdentical := true
	for _, name := range fileNames {
		primary, err := os.ReadFile(filepath.Join(*vectorsDir, name))
		if err != nil {
			fail("read %s: %v", name, err)
		}
		mirror, err := os.ReadFile(filepath.Join(*mirrorDir, name))
		if err != nil || !bytes.Equal(primary, mirror) {
			mirrorIdentical = false
			fmt.Printf("MIRROR MISMATCH %s\n", name)
		}
	}
	if provenance, err := os.ReadFile(filepath.Join(*vectorsDir, "PROVENANCE.json")); err == nil {
		mirror, mirrorErr := os.ReadFile(filepath.Join(*mirrorDir, "PROVENANCE.json"))
		if mirrorErr != nil || !bytes.Equal(provenance, mirror) {
			mirrorIdentical = false
			fmt.Println("MIRROR MISMATCH PROVENANCE.json")
		}
	}

	results := make([]vectorResult, 0, len(vectors))
	classCounts := map[string]int{}
	totalPass := 0
	fmt.Printf("decoder bomb court: %d vectors, repeat=%d, timeout=%ds\n", len(vectors), *repeat, *timeoutSecs)
	for _, vector := range vectors {
		root, err := os.MkdirTemp("", "weft-bomb-")
		if err != nil {
			fail("mktemp: %v", err)
		}
		inputPath, depsDir, sourceDir, outPathVector, outDir, filePaths, prepErr := prepare(vector, root)
		if prepErr != nil {
			fail("prepare %s: %v", vector.Name, prepErr)
		}
		args := vectorArgs(vector, inputPath, depsDir, sourceDir, outPathVector, outDir, filePaths)
		result := vectorResult{
			Name: vector.Name, Description: vector.Description, Verb: vector.Verb,
			ExpectedClass: vector.ExpectClass, Flags: vector.Flags,
		}
		vectorBytes, _ := os.ReadFile(filepath.Join(*vectorsDir, findFile(vector.Name, fileNames, vectors)))
		result.SHA256 = sha256Hex(vectorBytes)
		classCounts[vector.ExpectClass]++

		for run := 0; run < *repeat; run++ {
			rust := runOne(*rustCLI, args, root, outPathVector, vector.Verb == "archive-decode", *timeoutSecs)
			// Remove any output before the Go run so output evidence is per-CLI.
			_ = os.Remove(outPathVector)
			_ = os.RemoveAll(outDir)
			goRun := runOne(*goCLI, args, root, outPathVector, vector.Verb == "archive-decode", *timeoutSecs)
			_ = os.Remove(outPathVector)
			_ = os.RemoveAll(outDir)
			result.Rust = append(result.Rust, rust)
			result.Go = append(result.Go, goRun)
			fmt.Printf("%-42s run%d rust=%-18s go=%-18s rust_rss=%6dKiB go_rss=%6dKiB\n",
				vector.Name, run, classOr(rust), classOr(goRun), rust.MaxRSSKB, goRun.MaxRSSKB)
		}
		_ = os.RemoveAll(root)

		// Stability: every run of each CLI must report the same class.
		result.ClassesStable = true
		for _, run := range result.Rust {
			if run.Class != result.Rust[0].Class {
				result.ClassesStable = false
			}
		}
		for _, run := range result.Go {
			if run.Class != result.Go[0].Class {
				result.ClassesStable = false
			}
		}
		for index := range result.Rust {
			rust := result.Rust[index]
			goRun := result.Go[index]
			if rust.ExitCode == 0 {
				result.Failures = append(result.Failures, fmt.Sprintf("rust run %d exited 0", index))
			}
			if goRun.ExitCode == 0 {
				result.Failures = append(result.Failures, fmt.Sprintf("go run %d exited 0", index))
			}
			if rust.OOMKilled || rust.ExitCode == 137 || rust.TimedOut {
				result.Failures = append(result.Failures, fmt.Sprintf("rust run %d timed out or was OOM-killed", index))
			}
			if goRun.OOMKilled || goRun.ExitCode == 137 || goRun.TimedOut {
				result.Failures = append(result.Failures, fmt.Sprintf("go run %d timed out or was OOM-killed", index))
			}
			if rust.MaxRSSKB > *maxRSSLimit {
				result.Failures = append(result.Failures, fmt.Sprintf("rust run %d RSS %d KiB exceeds %d KiB", index, rust.MaxRSSKB, *maxRSSLimit))
			}
			if goRun.MaxRSSKB > *maxRSSLimit {
				result.Failures = append(result.Failures, fmt.Sprintf("go run %d RSS %d KiB exceeds %d KiB", index, goRun.MaxRSSKB, *maxRSSLimit))
			}
			if rust.Class != vector.ExpectClass {
				result.Failures = append(result.Failures, fmt.Sprintf("rust run %d class %q != %q", index, rust.Class, vector.ExpectClass))
			}
			if goRun.Class != vector.ExpectClass {
				result.Failures = append(result.Failures, fmt.Sprintf("go run %d class %q != %q", index, goRun.Class, vector.ExpectClass))
			}
			if rust.Class != goRun.Class {
				result.Failures = append(result.Failures, fmt.Sprintf("run %d class mismatch rust=%q go=%q", index, rust.Class, goRun.Class))
			}
			if rust.OutputMade {
				result.Failures = append(result.Failures, fmt.Sprintf("rust run %d created output", index))
			}
			if goRun.OutputMade {
				result.Failures = append(result.Failures, fmt.Sprintf("go run %d created output", index))
			}
		}
		if !result.ClassesStable {
			result.Failures = append(result.Failures, "classes were not stable across repeats")
		}
		result.Pass = len(result.Failures) == 0
		if result.Pass {
			totalPass++
		} else {
			fmt.Printf("FAIL %s: %s\n", vector.Name, strings.Join(result.Failures, "; "))
		}
		results = append(results, result)
	}

	provenanceBytes, _ := os.ReadFile(filepath.Join(*vectorsDir, "PROVENANCE.json"))
	revision := func(key string) string {
		if value := os.Getenv(key); value != "" {
			return value
		}
		return "unknown"
	}
	env := environment{
		Schema: "weftfs.decoder-bombs.receipt/1", GeneratedAt: *stamp, Court: "bench",
		Kernel: os.Getenv("WEFT_COURT_KERNEL"), Arch: runtime.GOARCH,
		Image: os.Getenv("WEFT_COURT_IMAGE"), ImageID: os.Getenv("WEFT_COURT_IMAGE_ID"),
		DockerServer: os.Getenv("WEFT_DOCKER_SERVER"), DockerClient: os.Getenv("WEFT_DOCKER_CLIENT"),
		Memory: os.Getenv("WEFT_COURT_MEMORY"), MemorySwap: os.Getenv("WEFT_COURT_MEMORY"),
		CPUs: os.Getenv("WEFT_COURT_CPUS"), PIDs: os.Getenv("WEFT_COURT_PIDS"),
		TmpfsTmp: "1g", GoVersion: runtime.Version(), RustcVersion: rustcVersion(),
		Repositories: map[string]string{
			"WeftFS":       revision("WEFT_REPO_WEFTFS"),
			"WeftCodec":    revision("WEFT_REPO_WEFTCODEC"),
			"WeftCodec-Go": revision("WEFT_REPO_WEFTCODEC_GO"),
			"WeftFS-Go":    revision("WEFT_REPO_WEFTFS_GO"),
		},
	}
	receiptValue := receipt{
		Schema: "weftfs.decoder-bombs.receipt/1", GeneratedAt: *stamp,
		Environment: env,
		Court: map[string]any{
			"kind": "bench", "memory": env.Memory, "memory_swap": env.MemorySwap,
			"cpus": env.CPUs, "pids": env.PIDs, "tmpfs_tmp": "1g",
			"cap_drop": "ALL", "no_new_privileges": true,
		},
		VectorsDir: *vectorsDir, MirrorDir: *mirrorDir,
		TimeoutSecs: *timeoutSecs, Repeat: *repeat, Results: results,
		Summary: summary{
			Total: len(results), Pass: totalPass, Fail: len(results) - totalPass,
			AllPass: totalPass == len(results) && mirrorIdentical,
			Classes: classCounts, MirrorIdentical: mirrorIdentical,
			VectorCountOK: len(results) >= 20, MinVectorCount: 20,
		},
	}
	_ = provenanceBytes
	encoded, err := json.MarshalIndent(receiptValue, "", "  ")
	if err != nil {
		fail("marshal receipt: %v", err)
	}
	if err := os.WriteFile(*outPath, append(encoded, '\n'), 0o644); err != nil {
		fail("write receipt: %v", err)
	}
	fmt.Printf("summary: pass=%d fail=%d mirror_identical=%v receipt=%s\n",
		totalPass, len(results)-totalPass, mirrorIdentical, *outPath)
	if totalPass != len(results) || !mirrorIdentical {
		os.Exit(1)
	}
}

func classOr(run cliRun) string {
	if run.Class != "" {
		return run.Class
	}
	if run.Signal != "" {
		return "signal:" + run.Signal
	}
	return fmt.Sprintf("exit:%d", run.ExitCode)
}

func findFile(name string, fileNames []string, vectors []Vector) string {
	for index, vector := range vectors {
		if vector.Name == name && index < len(fileNames) {
			return fileNames[index]
		}
	}
	return ""
}

func rustcVersion() string {
	cmd := exec.Command("rustc", "--version")
	out, err := cmd.Output()
	if err != nil {
		return "unknown"
	}
	return strings.TrimSpace(string(out))
}

// Keep time imported for the receipt's wall-time semantics documentation.
var _ = time.Second
