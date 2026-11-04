// Matched Rust/Go WeftCodec performance court.
//
// Both implementations are measured the same way: one CLI process per encode
// and one per decode, same fixtures, same flags, same court limits. Every
// decode is compared byte-for-byte with the expected plaintext before its
// sample is accepted. Wall time includes process spawn and dynamic loading;
// per-process CPU user/sys and max RSS come from wait4 rusage.
//
// This court is performance-only. Cross-decode correctness is already covered
// by the conformance courts; here it is re-checked for every frame so a timing
// sample cannot come from a failed decode.
//
// Run through scripts/docker-court.sh bench with
//
//	go run bench/matched/main.go --rust-cli ... --go-cli ... --out ...
package main

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"
)

type runResult struct {
	WallNS    int64 `json:"wall_ns"`
	UserNS    int64 `json:"cpu_user_ns"`
	SysNS     int64 `json:"cpu_sys_ns"`
	MaxRSSKiB int64 `json:"maxrss_kib"`
	ExitCode  int   `json:"exit_code"`
}

type fixture struct {
	Name   string `json:"name"`
	Path   string `json:"path"`
	Bytes  int64  `json:"bytes"`
	SHA256 string `json:"sha256"`
}

type modeSpec struct {
	Name       string
	Fixture    string
	Expected   string
	EncodeArgs []string
	NeedsDeps  bool
	DepFile    string
	Small      bool
}

func xorshift(state *uint64) uint64 {
	x := *state
	x ^= x << 13
	x ^= x >> 7
	x ^= x << 17
	*state = x
	return x
}

func randomBytes(n int, seed uint64) []byte {
	out := make([]byte, n)
	state := seed | 1
	for i := 0; i < n; i += 8 {
		v := xorshift(&state)
		var buf [8]byte
		binary.LittleEndian.PutUint64(buf[:], v)
		copy(out[i:], buf[:minInt(8, n-i)])
	}
	return out
}

func sourceText(n int) []byte {
	var b strings.Builder
	b.Grow(n + 256)
	for i := 0; b.Len() < n; i++ {
		fmt.Fprintf(&b, "weft-bench-source line %06d token=%08x payload=%016x repeated-block-abcdefghijklmnop\n",
			i, (i*2654435761)&0xffffffff, (uint64(i)*0x9e3779b97f4a7c15)&0xffffffffffffffff)
	}
	return []byte(b.String()[:n])
}

func nearDuplicate(base []byte, edits int) []byte {
	out := append([]byte(nil), base...)
	if len(out) == 0 {
		return out
	}
	state := uint64(0xDEADBEEF)
	for i := 0; i < edits; i++ {
		pos := int(xorshift(&state) % uint64(len(out)))
		out[pos] ^= byte(1 + (xorshift(&state) % 255))
	}
	return out
}

func tokenJSON(count int) []byte {
	var b strings.Builder
	b.WriteString(`{"vocab_schema":"bench-v1","token_ids":[`)
	state := uint64(0x1234ABCD)
	for i := 0; i < count; i++ {
		if i > 0 {
			b.WriteByte(',')
		}
		b.WriteString(strconv.FormatUint(xorshift(&state)%32000, 10))
	}
	b.WriteString(`]}`)
	return []byte(b.String())
}

func fileSHA256(path string) (string, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:]), nil
}

func timevalNS(tv syscall.Timeval) int64 {
	return tv.Sec*1e9 + tv.Usec*1e3
}

func run(bin string, args []string) (runResult, string, error) {
	cmd := exec.Command(bin, args...)
	var stderr strings.Builder
	cmd.Stdout = io.Discard
	cmd.Stderr = &stderr
	start := time.Now()
	err := cmd.Run()
	wall := time.Since(start).Nanoseconds()
	res := runResult{WallNS: wall, ExitCode: -1}
	if cmd.ProcessState != nil {
		res.ExitCode = cmd.ProcessState.ExitCode()
		if usage, ok := cmd.ProcessState.SysUsage().(*syscall.Rusage); ok {
			res.UserNS = timevalNS(usage.Utime)
			res.SysNS = timevalNS(usage.Stime)
			res.MaxRSSKiB = usage.Maxrss
		}
	}
	return res, stderr.String(), err
}

func mustRun(bin string, args []string, what string) runResult {
	res, stderr, err := run(bin, args)
	if err != nil {
		panic(fmt.Sprintf("%s failed (%s): %v: %s", what, strings.Join(args, " "), err, stderr))
	}
	return res
}

func stats(samples []int64) map[string]any {
	if len(samples) == 0 {
		return map[string]any{"samples": 0}
	}
	values := append([]int64(nil), samples...)
	sort.Slice(values, func(i, j int) bool { return values[i] < values[j] })
	pct := func(p int) int64 {
		if len(values) == 1 {
			return values[0]
		}
		rank := int(float64(p)/100.0*float64(len(values)-1) + 0.5)
		if rank < 0 {
			rank = 0
		}
		if rank >= len(values) {
			rank = len(values) - 1
		}
		return values[rank]
	}
	var sum int64
	for _, v := range values {
		sum += v
	}
	var median float64
	if len(values)%2 == 1 {
		median = float64(values[len(values)/2])
	} else {
		median = float64(values[len(values)/2-1]+values[len(values)/2]) / 2
	}
	return map[string]any{
		"samples": len(values),
		"min":     values[0],
		"median":  median,
		"mean":    float64(sum) / float64(len(values)),
		"p95":     pct(95),
		"max":     values[len(values)-1],
		"raw":     values,
	}
}

func env(key string) string { return os.Getenv(key) }

func cpuModel() string {
	data, err := os.ReadFile("/proc/cpuinfo")
	if err != nil {
		return ""
	}
	for _, line := range strings.Split(string(data), "\n") {
		if strings.HasPrefix(line, "model name") {
			if _, v, ok := strings.Cut(line, ":"); ok {
				return strings.TrimSpace(v)
			}
		}
	}
	return ""
}

func memTotalKiB() int64 {
	data, err := os.ReadFile("/proc/meminfo")
	if err != nil {
		return 0
	}
	for _, line := range strings.Split(string(data), "\n") {
		if strings.HasPrefix(line, "MemTotal:") {
			fields := strings.Fields(line)
			if len(fields) >= 2 {
				v, _ := strconv.ParseInt(fields[1], 10, 64)
				return v
			}
		}
	}
	return 0
}

func toolVersion(bin string, args ...string) string {
	out, err := exec.Command(bin, args...).CombinedOutput()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(strings.Split(string(out), "\n")[0])
}

type depInfo struct {
	Hash       string `json:"hash"`
	DecodedLen uint64 `json:"decoded_len"`
}

type inspectDoc struct {
	Mode         string    `json:"mode"`
	FrameLen     int64     `json:"frame_len"`
	DecodedLen   uint64    `json:"decoded_len"`
	Dependencies []depInfo `json:"dependencies"`
}

func inspectFrame(bin, frame string) inspectDoc {
	cmd := exec.Command(bin, "inspect", "--json", frame)
	out, err := cmd.Output()
	if err != nil {
		panic(fmt.Sprintf("inspect %s: %v", frame, err))
	}
	var doc inspectDoc
	if err := json.Unmarshal(out, &doc); err != nil {
		panic(fmt.Sprintf("parse inspect %s: %v: %s", frame, err, string(out)))
	}
	return doc
}

func buildDeps(bin, frame, depsDir string, sourceFiles map[string]string) []depInfo {
	doc := inspectFrame(bin, frame)
	if len(doc.Dependencies) == 0 {
		return nil
	}
	if err := os.MkdirAll(depsDir, 0o755); err != nil {
		panic(err)
	}
	for _, dep := range doc.Dependencies {
		matched := ""
		for _, path := range sourceFiles {
			info, err := os.Stat(path)
			if err == nil && uint64(info.Size()) == dep.DecodedLen {
				matched = path
				break
			}
		}
		if matched == "" {
			panic(fmt.Sprintf("no source file matches dependency %s (decoded_len=%d)", dep.Hash, dep.DecodedLen))
		}
		data, err := os.ReadFile(matched)
		if err != nil {
			panic(err)
		}
		if err := os.WriteFile(filepath.Join(depsDir, dep.Hash), data, 0o644); err != nil {
			panic(err)
		}
	}
	return doc.Dependencies
}

type contestantModeResult struct {
	Encode      map[string]any `json:"encode"`
	Decode      map[string]any `json:"decode"`
	FrameBytes  []int64        `json:"frame_bytes"`
	DecodeOK    bool           `json:"decode_verified"`
	ExpectedSHA string         `json:"expected_sha256"`
	FrameSHA    string         `json:"frame_sha256"`
}

type modeResult struct {
	Mode         string                          `json:"mode"`
	Fixture      string                          `json:"fixture"`
	ExpectedSHA  string                          `json:"expected_sha256"`
	Contestants  map[string]contestantModeResult `json:"contestants"`
	Dependencies map[string]int                  `json:"dependency_count"`
	CrossDecode  map[string]bool                 `json:"cross_decode"`
}

func equalBytes(a, b []byte) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func minInt(a, b int) int {
	if a < b {
		return a
	}
	return b
}

func main() {
	var rustCLI, goCLI, out, scratch string
	iterations := 7
	smallIterations := 15
	flag.StringVar(&rustCLI, "rust-cli", "", "Rust weftcodec CLI")
	flag.StringVar(&goCLI, "go-cli", "", "Go weftcodec CLI")
	flag.StringVar(&out, "out", "", "receipt path")
	flag.StringVar(&scratch, "scratch", "", "scratch directory")
	flag.IntVar(&iterations, "iterations", 7, "iterations for 1-4 MiB fixtures")
	flag.IntVar(&smallIterations, "small-iterations", 15, "iterations for small fixtures")
	flag.Parse()
	if rustCLI == "" || goCLI == "" || out == "" || scratch == "" {
		panic("--rust-cli, --go-cli, --out, --scratch are required")
	}
	if err := os.MkdirAll(scratch, 0o755); err != nil {
		panic(err)
	}

	fixturesDir := filepath.Join(scratch, "fixtures")
	if err := os.MkdirAll(fixturesDir, 0o755); err != nil {
		panic(err)
	}
	base := randomBytes(1<<20, 0xA11CE)
	fixtureBytes := map[string][]byte{
		"raw_4m":         randomBytes(4<<20, 0xBEEF),
		"zstd_4m":        sourceText(4 << 20),
		"ref_base_1m":    base,
		"ref_target_1m":  nearDuplicate(base, 10_000),
		"dict_64k":       sourceText(64 << 10),
		"dict_target_1m": sourceText(1 << 20),
		"tensor_f32_16k": randomBytes(4096, 0x7E4501),
		"tokens_4096":    tokenJSON(4096),
	}
	order := []string{"raw_4m", "zstd_4m", "ref_base_1m", "ref_target_1m", "dict_64k", "dict_target_1m", "tensor_f32_16k", "tokens_4096"}
	var fixtureManifest []fixture
	paths := map[string]string{}
	for _, name := range order {
		data := fixtureBytes[name]
		path := filepath.Join(fixturesDir, name)
		if err := os.WriteFile(path, data, 0o644); err != nil {
			panic(err)
		}
		sum := sha256.Sum256(data)
		paths[name] = path
		fixtureManifest = append(fixtureManifest, fixture{
			Name: name, Path: path, Bytes: int64(len(data)), SHA256: hex.EncodeToString(sum[:]),
		})
	}

	modes := []modeSpec{
		{Name: "raw", Fixture: "raw_4m", Expected: "raw_4m", EncodeArgs: []string{"--mode", "raw"}},
		{Name: "zstd", Fixture: "zstd_4m", Expected: "zstd_4m", EncodeArgs: []string{"--mode", "zstd", "--level", "3"}},
		{Name: "reference", Fixture: "ref_base_1m", Expected: "ref_base_1m", EncodeArgs: []string{"--mode", "reference", "--base", paths["ref_base_1m"]}, NeedsDeps: true, DepFile: "ref_base_1m"},
		{Name: "delta", Fixture: "ref_target_1m", Expected: "ref_target_1m", EncodeArgs: []string{"--mode", "delta", "--base", paths["ref_base_1m"]}, NeedsDeps: true, DepFile: "ref_base_1m"},
		{Name: "dictionary", Fixture: "dict_target_1m", Expected: "dict_target_1m", EncodeArgs: []string{"--mode", "dictionary", "--dict", paths["dict_64k"], "--level", "3"}, NeedsDeps: true, DepFile: "dict_64k"},
		{Name: "tensor", Fixture: "tensor_f32_16k", Expected: "tensor_f32_16k", EncodeArgs: []string{"--mode", "tensor", "--dtype", "f32", "--shape", "1024"}, Small: true},
		{Name: "tokens", Fixture: "tokens_4096", Expected: "tokens_4096", EncodeArgs: []string{"--mode", "tokens"}, Small: true},
	}

	contestants := []struct {
		ID  string
		Bin string
	}{{"rust", rustCLI}, {"go", goCLI}}

	var results []modeResult
	for _, spec := range modes {
		iters := iterations
		if spec.Small {
			iters = smallIterations
		}
		expectedBytes := fixtureBytes[spec.Expected]
		if spec.Name == "tokens" {
			// TOKEN_SEQUENCE decode returns the canonical token payload, not the
			// JSON input. Capture the first verified decode as the expected
			// bytes and require the other implementation to match exactly.
			expectedBytes = nil
		}
		expectedHex := ""
		if expectedBytes != nil {
			expectedSum := sha256.Sum256(expectedBytes)
			expectedHex = hex.EncodeToString(expectedSum[:])
		}
		result := modeResult{
			Mode: spec.Name, Fixture: spec.Fixture, ExpectedSHA: expectedHex,
			Contestants:  map[string]contestantModeResult{},
			Dependencies: map[string]int{},
			CrossDecode:  map[string]bool{},
		}
		framePaths := map[string]string{}
		depsDirs := map[string]string{}
		for _, c := range contestants {
			modeScratch := filepath.Join(scratch, "runs", spec.Name, c.ID)
			if err := os.MkdirAll(modeScratch, 0o755); err != nil {
				panic(err)
			}
			frame := filepath.Join(modeScratch, "frame.wft")
			decoded := filepath.Join(modeScratch, "decoded.bin")
			depsDir := filepath.Join(modeScratch, "deps")
			encodeArgs := append([]string{"encode", "--input", paths[spec.Fixture], "--output", frame}, spec.EncodeArgs...)
			mustRun(c.Bin, encodeArgs, spec.Name+"/"+c.ID+" encode")
			var deps []depInfo
			if spec.NeedsDeps {
				deps = buildDeps(c.Bin, frame, depsDir, map[string]string{spec.DepFile: paths[spec.DepFile]})
			}
			decodeArgs := []string{"decode", "--input", frame, "--output", decoded}
			if spec.NeedsDeps {
				decodeArgs = append(decodeArgs, "--deps", depsDir)
			}
			mustRun(c.Bin, decodeArgs, spec.Name+"/"+c.ID+" decode")
			got, err := os.ReadFile(decoded)
			if err != nil {
				panic(err)
			}
			if expectedBytes == nil {
				expectedBytes = append([]byte(nil), got...)
				expectedSum := sha256.Sum256(expectedBytes)
				expectedHex = hex.EncodeToString(expectedSum[:])
				result.ExpectedSHA = expectedHex
			}
			if !equalBytes(got, expectedBytes) {
				panic(fmt.Sprintf("%s/%s initial decode mismatch: got %d bytes, want %d", spec.Name, c.ID, len(got), len(expectedBytes)))
			}
			framePaths[c.ID] = frame
			depsDirs[c.ID] = depsDir
			result.Dependencies[c.ID] = len(deps)

			var encodeSamples, decodeSamples []int64
			var encodeUser, encodeSys, decodeUser, decodeSys []int64
			var encodeCPU, decodeCPU []int64
			var encodeRSS, decodeRSS []int64
			var frameSizes []int64
			verified := true
			for i := 0; i < iters; i++ {
				er, stderr, err := run(c.Bin, encodeArgs)
				if err != nil {
					panic(fmt.Sprintf("%s/%s encode iter %d: %v: %s", spec.Name, c.ID, i, err, stderr))
				}
				encodeSamples = append(encodeSamples, er.WallNS)
				encodeUser = append(encodeUser, er.UserNS)
				encodeSys = append(encodeSys, er.SysNS)
				encodeCPU = append(encodeCPU, er.UserNS+er.SysNS)
				encodeRSS = append(encodeRSS, er.MaxRSSKiB)
				info, err := os.Stat(frame)
				if err != nil {
					panic(err)
				}
				frameSizes = append(frameSizes, info.Size())

				dr, stderr, err := run(c.Bin, decodeArgs)
				if err != nil {
					panic(fmt.Sprintf("%s/%s decode iter %d: %v: %s", spec.Name, c.ID, i, err, stderr))
				}
				decodeSamples = append(decodeSamples, dr.WallNS)
				decodeUser = append(decodeUser, dr.UserNS)
				decodeSys = append(decodeSys, dr.SysNS)
				decodeCPU = append(decodeCPU, dr.UserNS+dr.SysNS)
				decodeRSS = append(decodeRSS, dr.MaxRSSKiB)
				plain, err := os.ReadFile(decoded)
				if err != nil {
					panic(err)
				}
				if !equalBytes(plain, expectedBytes) {
					verified = false
					break
				}
			}
			if !verified {
				panic(fmt.Sprintf("%s/%s: a timed decode failed verification", spec.Name, c.ID))
			}
			frameSum, err := fileSHA256(frame)
			if err != nil {
				panic(err)
			}
			result.Contestants[c.ID] = contestantModeResult{
				Encode: map[string]any{
					"wall": stats(encodeSamples), "cpu_user": stats(encodeUser), "cpu_sys": stats(encodeSys),
					"cpu_total": stats(encodeCPU), "maxrss_kib": stats(encodeRSS),
				},
				Decode: map[string]any{
					"wall": stats(decodeSamples), "cpu_user": stats(decodeUser), "cpu_sys": stats(decodeSys),
					"cpu_total": stats(decodeCPU), "maxrss_kib": stats(decodeRSS),
				},
				FrameBytes: frameSizes, DecodeOK: verified, ExpectedSHA: expectedHex, FrameSHA: frameSum,
			}
		}

		for _, pair := range [][2]string{{"rust", "go"}, {"go", "rust"}} {
			src, dst := pair[0], pair[1]
			decoded := filepath.Join(scratch, "runs", spec.Name, src+"-by-"+dst+".bin")
			args := []string{"decode", "--input", framePaths[src], "--output", decoded}
			if spec.NeedsDeps {
				args = append(args, "--deps", depsDirs[src])
			}
			bin := goCLI
			if dst == "rust" {
				bin = rustCLI
			}
			mustRun(bin, args, spec.Name+"/"+src+"-by-"+dst+" cross decode")
			plain, err := os.ReadFile(decoded)
			if err != nil {
				panic(err)
			}
			result.CrossDecode[src+"_frame_by_"+dst] = equalBytes(plain, expectedBytes)
		}
		results = append(results, result)
	}

	type overhead struct {
		Wall    map[string]any `json:"wall"`
		CPUUser map[string]any `json:"cpu_user"`
		CPUSys  map[string]any `json:"cpu_sys"`
		RSS     map[string]any `json:"maxrss_kib"`
	}
	spawn := map[string]overhead{}
	for _, c := range contestants {
		var wall, user, sys, rss []int64
		for i := 0; i < 15; i++ {
			res, _, _ := run(c.Bin, []string{"--help"})
			wall = append(wall, res.WallNS)
			user = append(user, res.UserNS)
			sys = append(sys, res.SysNS)
			rss = append(rss, res.MaxRSSKiB)
		}
		spawn[c.ID] = overhead{Wall: stats(wall), CPUUser: stats(user), CPUSys: stats(sys), RSS: stats(rss)}
	}

	type contestantInfo struct {
		ID         string `json:"id"`
		Binary     string `json:"binary"`
		BinarySize int64  `json:"binary_bytes"`
		BinarySHA  string `json:"binary_sha256"`
	}
	var cinfo []contestantInfo
	for _, c := range contestants {
		info, err := os.Stat(c.Bin)
		if err != nil {
			panic(err)
		}
		sum, err := fileSHA256(c.Bin)
		if err != nil {
			panic(err)
		}
		cinfo = append(cinfo, contestantInfo{ID: c.ID, Binary: c.Bin, BinarySize: info.Size(), BinarySHA: sum})
	}

	report := map[string]any{
		"schema": "weft-codec-matched-bench-v1",
		"kind":   "matched-go-rust-codec-performance",
		"method": map[string]any{
			"execution":          "one CLI process per encode and per decode for both implementations",
			"wall_includes":      "process spawn, dynamic loading, argument parsing, file I/O, and codec work",
			"cpu_source":         "wait4 rusage (user + system CPU time of the child process); cpu_total is their per-sample sum",
			"cpu_note":           "Go runtime threads can make process CPU time exceed wall time; CPU time is not energy",
			"rss_source":         "wait4 rusage ru_maxrss in KiB",
			"units":              "wall/cpu values are nanoseconds; maxrss values are KiB",
			"verification":       "every timed decode is compared byte-for-byte with the fixture before its sample is accepted",
			"cross_decode":       "each frame is decoded by the other implementation and compared with the same expected bytes",
			"limits":             "both CLIs use the default normative Limits; no limit flag is set",
			"iterations_large":   iterations,
			"iterations_small":   smallIterations,
			"process_overhead":   "measured separately with --help; not subtracted from encode/decode wall times",
			"library_comparison": "no library harness is used in this court; the existing in-process Rust codec receipt uses a different corpus and is not directly comparable",
		},
		"fixtures":         fixtureManifest,
		"contestants":      cinfo,
		"modes":            results,
		"process_overhead": spawn,
		"environment": map[string]any{
			"kernel":                toolVersion("uname", "-r"),
			"uname":                 toolVersion("uname", "-a"),
			"cpu_model":             cpuModel(),
			"nproc":                 runtime.NumCPU(),
			"mem_total_kib":         memTotalKiB(),
			"rustc":                 toolVersion("rustc", "--version"),
			"go":                    toolVersion("go", "version"),
			"court_image":           env("WEFT_COURT_IMAGE"),
			"court_image_id":        env("WEFT_COURT_IMAGE_ID"),
			"court_memory":          env("WEFT_COURT_MEMORY"),
			"court_cpus":            env("WEFT_COURT_CPUS"),
			"court_pids":            env("WEFT_COURT_PIDS"),
			"docker_client":         env("WEFT_DOCKER_CLIENT"),
			"docker_server":         env("WEFT_DOCKER_SERVER"),
			"revision_weftfs":       env("WEFT_REPO_WEFTFS"),
			"revision_weftfs_go":    env("WEFT_REPO_WEFTFS_GO"),
			"revision_weftcodec":    env("WEFT_REPO_WEFTCODEC"),
			"revision_weftcodec_go": env("WEFT_REPO_WEFTCODEC_GO"),
		},
		"notes": []string{
			"Both contestants are CLI processes; no library harness is used, so wall times include a constant process overhead measured separately.",
			"CPU user/sys is CPU time, not energy. No perf counters or RAPL reading is claimed.",
			"TENSOR and TOKENS expected bytes are the raw payload and the canonical token payload respectively; both implementations must decode to the same bytes.",
			"Cross-decode is re-checked here for every frame; it is performance evidence, not a conformance court.",
			"No result is extrapolated to a different corpus, mount, or machine.",
		},
	}

	text, err := json.MarshalIndent(report, "", "  ")
	if err != nil {
		panic(err)
	}
	if err := os.WriteFile(out, text, 0o644); err != nil {
		panic(err)
	}
	fmt.Println("wrote", out)
}
