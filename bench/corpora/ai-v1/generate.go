// Command generate builds the deterministic synthetic AI corpus under
// bench/corpora/ai-v1. It downloads nothing and uses only the Go standard
// library.
//
// Run inside the bench court:
//
//	cd bench/corpora/ai-v1 && GO111MODULE=off go run generate.go
//
// Every byte is derived from a fixed seed. The manifest records per-file
// SHA-256, source bytes, canonical visible bytes, license, and provenance.
package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

const (
	corpusSchema = "weft.ai-corpus.v1"
	corpusName   = "ai-v1"
	license      = "CC0-1.0"
	provenance   = "Synthetic. Generated locally from a fixed seed by bench/corpora/ai-v1/generate.go. No downloaded, scraped, production, or Meta data is present. Values are patterns chosen to exercise near-duplicate, tensor, token, and checkpoint paths; they have no scientific meaning."
	seedValue    = 0x9E3779B97F4A7C15
	createdUTC   = "2026-11-03T00:00:00Z"
)

type rng struct{ state uint64 }

func newRNG(seed uint64) *rng { return &rng{state: seed | 1} }

func (r *rng) next() uint64 {
	x := r.state
	x ^= x << 13
	x ^= x >> 7
	x ^= x << 17
	r.state = x
	return x
}

func (r *rng) byte() byte { return byte(r.next()) }

type relation struct {
	Predicate string `json:"predicate"`
	Target    string `json:"target"`
}

type tensorMeta struct {
	Dtype string   `json:"dtype"`
	Shape []uint64 `json:"shape"`
}

type tokenMeta struct {
	VocabSchema  string `json:"vocab_schema"`
	TokenCount   uint64 `json:"token_count"`
	PayloadBytes uint64 `json:"payload_bytes"`
}

type fileEntry struct {
	Path         string      `json:"path"`
	Kind         string      `json:"kind"`
	Bytes        uint64      `json:"source_bytes"`
	VisibleBytes uint64      `json:"visible_bytes"`
	SHA256       string      `json:"sha256"`
	Relation     *relation   `json:"relation,omitempty"`
	Tensor       *tensorMeta `json:"tensor,omitempty"`
	Tokens       *tokenMeta  `json:"tokens,omitempty"`
	Notes        string      `json:"notes,omitempty"`
}

type manifest struct {
	Schema      string      `json:"schema"`
	Corpus      string      `json:"corpus"`
	License     string      `json:"license"`
	LicenseNote string      `json:"license_note"`
	Provenance  string      `json:"provenance"`
	Generator   string      `json:"generator"`
	Seed        string      `json:"seed"`
	CreatedUTC  string      `json:"created_utc"`
	Files       []fileEntry `json:"files"`
	Totals      totals      `json:"totals"`
}

type totals struct {
	Files        int    `json:"files"`
	SourceBytes  uint64 `json:"source_bytes"`
	VisibleBytes uint64 `json:"visible_bytes"`
}

type pending struct {
	path     string
	kind     string
	data     []byte
	visible  uint64
	relation *relation
	tensor   *tensorMeta
	tokens   *tokenMeta
	notes    string
}

func main() {
	out := flag.String("out", ".", "output directory")
	check := flag.Bool("check", false, "verify an existing corpus instead of writing")
	flag.Parse()
	if *check {
		if err := verify(*out); err != nil {
			fmt.Fprintln(os.Stderr, "check failed:", err)
			os.Exit(1)
		}
		fmt.Println("corpus check: OK")
		return
	}
	if err := generate(*out); err != nil {
		fmt.Fprintln(os.Stderr, "generate failed:", err)
		os.Exit(1)
	}
	fmt.Println("wrote corpus to", *out)
}

func generate(root string) error {
	var files []pending
	r := newRNG(seedValue)

	// Prompts: text with a stable task/model/tool vocabulary.
	subjects := []string{
		"resolve the selector cardinality for a task context",
		"replay an agent bundle from a pinned snapshot",
		"materialize a training slice under a read budget",
		"explain why a delta base was rejected",
		"compare two checkpoint revisions byte-for-byte",
		"verify that a tensor descriptor matches its payload",
		"summarize a tool output without losing field names",
		"list objects grounded in one prompt",
		"measure read latency for a historical version",
		"audit a capability grant against a selector",
		"re-encode a token sequence after a vocabulary change",
		"check that a manifest hash covers every chunk",
	}
	verbs := []string{"summarize", "inspect", "verify", "replay", "materialize", "compare"}
	for i := 0; i < len(subjects); i++ {
		var b strings.Builder
		fmt.Fprintf(&b, "system: you are a deterministic coding agent; context is an address, not a cache key.\n")
		fmt.Fprintf(&b, "task: %s\n", subjects[i])
		fmt.Fprintf(&b, "model: weft-synthetic-%d\n", 7+i%3)
		for j := 0; j < 6; j++ {
			fmt.Fprintf(&b, "step %02d: %s object %08x and record the sha256.\n",
				j, verbs[(i+j)%len(verbs)], uint32(r.next()))
		}
		fmt.Fprintf(&b, "constraint: fail closed; never substitute a base.\n")
		path := fmt.Sprintf("prompts/p%03d.txt", i)
		var rel *relation
		if i > 0 && i%4 == 0 {
			rel = &relation{Predicate: "revised_from", Target: fmt.Sprintf("prompts/p%03d.txt", i-1)}
		}
		files = append(files, pending{path: path, kind: "prompt", data: []byte(b.String()), relation: rel})
	}

	// Tool outputs: JSON and plain text. Every third output is a near-duplicate
	// of the previous one and declares derived_from; every fifth is text.
	for i := 0; i < 12; i++ {
		textKind := i%5 == 4
		ext := "json"
		if textKind {
			ext = "txt"
		}
		path := fmt.Sprintf("tool_outputs/t%03d.%s", i, ext)
		var data []byte
		var rel *relation
		if i%3 == 2 {
			prev := &files[len(files)-1]
			edited := append([]byte(nil), prev.data...)
			replaceAt(edited, int(r.next()%uint64(len(edited)-16)), []byte(`status: revised`))
			data = edited
			rel = &relation{Predicate: "derived_from", Target: prev.path}
		} else if textKind {
			var b strings.Builder
			fmt.Fprintf(&b, "tool: weft-synthetic-%d\n", i%4)
			fmt.Fprintf(&b, "grounded_in: prompts/p%03d.txt\n", i%len(subjects))
			fmt.Fprintf(&b, "status: ok\nrecords:\n")
			for j := 0; j < 24; j++ {
				fmt.Fprintf(&b, "  - id=%d path=objects/%02x/%08x bytes=%d ok=true\n",
					j, uint32(r.next())&0xff, uint32(r.next()), 256+j*13)
			}
			fmt.Fprintf(&b, "summary: tool output %d: %s\n", i, subjects[i%len(subjects)])
			data = []byte(b.String())
		} else {
			var b strings.Builder
			fmt.Fprintf(&b, "{\n  \"tool\": \"weft-synthetic-%d\",\n", i%4)
			fmt.Fprintf(&b, "  \"grounded_in\": \"prompts/p%03d.txt\",\n", i%len(subjects))
			fmt.Fprintf(&b, "  \"status\": \"ok\",\n  \"records\": [\n")
			for j := 0; j < 24; j++ {
				comma := ","
				if j == 23 {
					comma = ""
				}
				fmt.Fprintf(&b, "    {\"id\": %d, \"path\": \"objects/%02x/%08x\", \"bytes\": %d, \"ok\": true}%s\n",
					j, uint32(r.next())&0xff, uint32(r.next()), 256+j*13, comma)
			}
			fmt.Fprintf(&b, "  ],\n  \"summary\": \"tool output %d: %s\"\n}\n", i, subjects[i%len(subjects)])
			data = []byte(b.String())
		}
		files = append(files, pending{path: path, kind: "tool_output", data: data, relation: rel})
	}

	// Agent intermediates: a 48 KiB base and 9 near-duplicate revisions. Each
	// revision derives from the previous revision (a chain for dependency
	// depth).
	agentBase := agentIntermediate(48<<10, r)
	files = append(files, pending{path: "agent_intermediates/a000_base.bin", kind: "agent_intermediate", data: agentBase})
	prev := agentBase
	prevPath := "agent_intermediates/a000_base.bin"
	// Eight revisions keep the chain exactly at the default codec dependency
	// depth limit (8). Deeper chains are a separate limit test, not a corpus
	// assumption.
	for i := 1; i <= 8; i++ {
		next := append([]byte(nil), prev...)
		// A handful of localized edits.
		for e := 0; e < 7; e++ {
			pos := int(r.next() % uint64(len(next)-64))
			copy(next[pos:pos+32], []byte(fmt.Sprintf("REV%02d-%02d-EDITED-BLOCK------------", i, e)))
		}
		// Append one new line.
		next = append(next, []byte(fmt.Sprintf("appended revision %02d marker %08x\n", i, uint32(r.next())))...)
		path := fmt.Sprintf("agent_intermediates/a%03d.bin", i)
		files = append(files, pending{
			path:     path,
			kind:     "agent_intermediate",
			data:     next,
			relation: &relation{Predicate: "derived_from", Target: prevPath},
		})
		prev = next
		prevPath = path
	}

	// Small tensors with realistic dtypes and shapes.
	tensors := []struct {
		name  string
		dtype string
		shape []uint64
		mode  string
	}{
		{"f32_64x64", "f32", []uint64{64, 64}, "smooth"},
		{"f32_128x64", "f32", []uint64{128, 64}, "random"},
		{"f16_64x64", "f16", []uint64{64, 64}, "smooth"},
		{"bf16_32x32", "bf16", []uint64{32, 32}, "random"},
		{"f64_16x16", "f64", []uint64{16, 16}, "smooth"},
		{"i8_1024", "i8", []uint64{1024}, "random"},
		{"u8_4096", "u8", []uint64{4096}, "smooth"},
		{"i16_512", "i16", []uint64{512}, "random"},
		{"i32_256", "i32", []uint64{256}, "smooth"},
		{"i64_128", "i64", []uint64{128}, "random"},
		{"f32_zero", "f32", []uint64{0, 16}, "zero"},
		{"f32_scalar", "f32", []uint64{}, "scalar"},
	}
	for _, t := range tensors {
		payload := tensorBytes(t.dtype, t.shape, t.mode, r)
		path := fmt.Sprintf("tensors/%s.bin", t.name)
		files = append(files, pending{
			path:    path,
			kind:    "tensor",
			data:    payload,
			visible: uint64(len(payload)),
			tensor:  &tensorMeta{Dtype: t.dtype, Shape: t.shape},
			notes:   "raw little-endian element bytes; dtype/shape in manifest",
		})
	}

	// Token sequences. The JSON file is the CLI input format; the manifest
	// records the canonical encoded payload length as visible_bytes.
	tokenSets := []struct {
		name string
		ids  []uint64
	}{
		{"empty", []uint64{}},
		{"single", []uint64{42}},
		{"boundaries", []uint64{0, 1, 127, 128, 255, 256, 65535, 65536, 1<<32 - 1, 1 << 32, 1<<63 - 1, ^uint64(0)}},
		{"decreasing", decreasing(1000)},
		{"walk_4096", walk(4096, r)},
		{"random_8192", randomTokens(8192, r)},
		{"large_20000", walk(20000, r)},
		{"repeated_1000", repeatToken(7, 1000)},
	}
	for _, set := range tokenSets {
		doc := tokenDocument{Schema: "weft.tokens.input.v1", VocabSchema: "weft.synthetic.vocab.v1", TokenIDs: set.ids}
		data, err := json.MarshalIndent(doc, "", "  ")
		if err != nil {
			return err
		}
		data = append(data, '\n')
		files = append(files, pending{
			path:    fmt.Sprintf("tokens/%s.json", set.name),
			kind:    "tokens",
			data:    data,
			visible: canonicalTokenPayloadLen(set.ids),
			tokens: &tokenMeta{
				VocabSchema:  doc.VocabSchema,
				TokenCount:   uint64(len(set.ids)),
				PayloadBytes: canonicalTokenPayloadLen(set.ids),
			},
			notes: "JSON is the CLI input form; visible_bytes is the canonical delta-LEB128 payload length",
		})
	}

	// Model-checkpoint-like blobs: a 128 KiB base and 6 revisions in a chain.
	checkBase := checkpointBytes(128<<10, r)
	files = append(files, pending{path: "checkpoints/c000_base.bin", kind: "checkpoint", data: checkBase})
	prev = checkBase
	prevPath = "checkpoints/c000_base.bin"
	for i := 1; i <= 6; i++ {
		next := append([]byte(nil), prev...)
		edits := len(next) / 333 // roughly 0.3% single-byte edits
		for e := 0; e < edits; e++ {
			pos := int(r.next() % uint64(len(next)))
			next[pos] ^= byte(1 + r.next()%251)
		}
		next = append(next, checkpointBytes(512, r)...)
		path := fmt.Sprintf("checkpoints/c%03d.bin", i)
		files = append(files, pending{
			path:     path,
			kind:     "checkpoint",
			data:     next,
			relation: &relation{Predicate: "derived_from", Target: prevPath},
		})
		prev = next
		prevPath = path
	}

	if err := os.MkdirAll(root, 0o755); err != nil {
		return err
	}
	// Remove previously generated content so the directory is exactly the
	// current manifest (no stale files from an earlier corpus revision).
	for _, sub := range []string{
		"prompts", "tool_outputs", "agent_intermediates",
		"tensors", "tokens", "checkpoints",
	} {
		if err := os.RemoveAll(filepath.Join(root, sub)); err != nil {
			return err
		}
	}
	entries := make([]fileEntry, 0, len(files))
	for _, f := range files {
		full := filepath.Join(root, filepath.FromSlash(f.path))
		if err := os.MkdirAll(filepath.Dir(full), 0o755); err != nil {
			return err
		}
		if err := os.WriteFile(full, f.data, 0o644); err != nil {
			return err
		}
		visible := f.visible
		if visible == 0 && f.tokens == nil {
			visible = uint64(len(f.data))
		}
		sum := sha256.Sum256(f.data)
		entries = append(entries, fileEntry{
			Path:         f.path,
			Kind:         f.kind,
			Bytes:        uint64(len(f.data)),
			VisibleBytes: visible,
			SHA256:       hex.EncodeToString(sum[:]),
			Relation:     f.relation,
			Tensor:       f.tensor,
			Tokens:       f.tokens,
			Notes:        f.notes,
		})
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i].Path < entries[j].Path })

	m := manifest{
		Schema:      corpusSchema,
		Corpus:      corpusName,
		License:     license,
		LicenseNote: "Synthetic corpus authored for this repository and released under CC0-1.0. It contains no third-party or production data.",
		Provenance:  provenance,
		Generator:   "bench/corpora/ai-v1/generate.go",
		Seed:        fmt.Sprintf("0x%016x", uint64(seedValue)),
		CreatedUTC:  createdUTC,
		Files:       entries,
	}
	for _, e := range entries {
		m.Totals.Files++
		m.Totals.SourceBytes += e.Bytes
		m.Totals.VisibleBytes += e.VisibleBytes
	}
	body, err := json.MarshalIndent(m, "", "  ")
	if err != nil {
		return err
	}
	body = append(body, '\n')
	if err := os.WriteFile(filepath.Join(root, "MANIFEST.json"), body, 0o644); err != nil {
		return err
	}
	note := fmt.Sprintf(`# ai-v1 synthetic corpus — license and provenance

- License: %s. The corpus is synthetic and authored by this repository's
  generator; it contains no downloaded, scraped, production, or Meta data.
- Generator: `+"`bench/corpora/ai-v1/generate.go`"+`, fixed seed %s.
- Every file is deterministic from that seed. `+"`MANIFEST.json`"+` records the
  per-file SHA-256, source bytes, canonical visible bytes, typed metadata, and
  declared relations.
- Values have no scientific meaning. Tensors and token sequences exercise
  dtype/shape validation and delta boundaries; prompts, tool outputs, agent
  intermediates, and checkpoints exercise textual and near-duplicate paths.
- This file is generated. Re-running the generator must reproduce the same
  bytes and hashes.
`, license, fmt.Sprintf("0x%016x", uint64(seedValue)))
	if err := os.WriteFile(filepath.Join(root, "LICENSE-NOTE.md"), []byte(note), 0o644); err != nil {
		return err
	}
	return nil
}

type tokenDocument struct {
	Schema      string   `json:"schema"`
	VocabSchema string   `json:"vocab_schema"`
	TokenIDs    []uint64 `json:"token_ids"`
}

func verify(root string) error {
	body, err := os.ReadFile(filepath.Join(root, "MANIFEST.json"))
	if err != nil {
		return err
	}
	var m manifest
	if err := json.Unmarshal(body, &m); err != nil {
		return err
	}
	for _, e := range m.Files {
		data, err := os.ReadFile(filepath.Join(root, filepath.FromSlash(e.Path)))
		if err != nil {
			return err
		}
		sum := sha256.Sum256(data)
		if got := hex.EncodeToString(sum[:]); got != e.SHA256 {
			return fmt.Errorf("%s: sha256 %s != manifest %s", e.Path, got, e.SHA256)
		}
		if uint64(len(data)) != e.Bytes {
			return fmt.Errorf("%s: %d bytes != manifest %d", e.Path, len(data), e.Bytes)
		}
	}
	return nil
}

func replaceAt(data []byte, pos int, text []byte) {
	if pos < 0 {
		pos = 0
	}
	end := pos + len(text)
	if end > len(data) {
		end = len(data)
		text = text[:end-pos]
	}
	copy(data[pos:end], text)
}

func agentIntermediate(size int, r *rng) []byte {
	lines := []string{
		"thought: the context root pins a snapshot; the selector identifies the query",
		"action: resolve dependency frames in topological order",
		"observation: sha256 matches the declared manifest entry",
		"action: compute the delta against the previous revision",
		"observation: the token payload round-trips exactly",
	}
	var b []byte
	for len(b) < size {
		line := fmt.Sprintf("%06d %s // nonce %08x\n", len(b), lines[int(r.next()%uint64(len(lines)))], uint32(r.next()))
		b = append(b, line...)
	}
	return b[:size]
}

func checkpointBytes(size int, r *rng) []byte {
	out := make([]byte, 0, size)
	block := make([]byte, 4096)
	for i := range block {
		block[i] = byte(i * 31)
	}
	for len(out) < size {
		// Mix repeated structured blocks with deterministic noise so a byte
		// delta has both long copies and insertions to find.
		if r.next()%5 == 0 {
			start := len(out)
			out = append(out, block...)
			for i := 0; i < 64 && start+i < len(out); i++ {
				out[start+i] ^= byte(r.next())
			}
		} else {
			for i := 0; i < 512; i++ {
				out = append(out, byte(r.next()))
			}
		}
	}
	return out[:size]
}

func tensorBytes(dtype string, shape []uint64, mode string, r *rng) []byte {
	size := elementSize(dtype)
	count := uint64(1)
	for _, d := range shape {
		count *= d
	}
	n := int(count * size)
	out := make([]byte, n)
	switch mode {
	case "zero":
		return out
	case "scalar":
		if n >= 4 {
			bits := math.Float32bits(1.0)
			out[0] = byte(bits)
			out[1] = byte(bits >> 8)
			out[2] = byte(bits >> 16)
			out[3] = byte(bits >> 24)
		}
		return out
	case "smooth":
		for i := 0; i < int(count); i++ {
			v := int64(i%97) - 48 + int64(r.next()%7)
			writeElement(out[i*int(size):], dtype, v, i)
		}
	default:
		for i := 0; i < n; i++ {
			out[i] = byte(r.next())
		}
	}
	// Add a repeated row for the smooth tensors so zstd has a local win too.
	if mode == "smooth" && len(shape) == 2 && shape[1] > 0 {
		row := int(size * shape[1])
		for i := 0; i+row < n && i < row*4; i += row {
			copy(out[i+row:i+2*row], out[i:i+row])
		}
	}
	return out
}

func elementSize(dtype string) uint64 {
	switch dtype {
	case "f16", "bf16", "i16":
		return 2
	case "f32", "i32":
		return 4
	case "f64", "i64":
		return 8
	default:
		return 1
	}
}

func writeElement(dst []byte, dtype string, value int64, i int) {
	var raw uint64
	switch dtype {
	case "f32":
		raw = uint64(math.Float32bits(float32(value) + float32(i%13)*0.25))
	case "f64":
		raw = math.Float64bits(float64(value) + float64(i%13)*0.25)
	case "f16", "bf16":
		raw = uint64(uint16((value*257 + int64(i)%31) & 0xffff))
	default:
		raw = uint64(value)
	}
	for b := uint64(0); b < elementSize(dtype); b++ {
		dst[b] = byte(raw >> (8 * b))
	}
}

func decreasing(n int) []uint64 {
	out := make([]uint64, 0, n)
	for i := n - 1; i >= 0; i-- {
		out = append(out, uint64(i))
	}
	return out
}

func walk(n int, r *rng) []uint64 {
	out := make([]uint64, 0, n)
	var v uint64
	for i := 0; i < n; i++ {
		delta := int64(r.next()%17) - 8
		v = uint64(int64(v) + delta)
		out = append(out, v)
	}
	return out
}

func randomTokens(n int, r *rng) []uint64 {
	out := make([]uint64, 0, n)
	for i := 0; i < n; i++ {
		out = append(out, r.next()%32000)
	}
	return out
}

func repeatToken(v uint64, n int) []uint64 {
	out := make([]uint64, n)
	for i := range out {
		out[i] = v
	}
	return out
}

// canonicalTokenPayloadLen computes the byte length of the normative
// u64-zigzag-delta-leb128 payload used by mode 7.
func canonicalTokenPayloadLen(ids []uint64) uint64 {
	var total uint64
	var previous uint64
	for i, id := range ids {
		var v uint64
		if i == 0 {
			v = id
		} else {
			delta := id - previous
			v = (delta << 1) ^ uint64(int64(delta)>>63)
		}
		total += uint64(uvarintLen(v))
		previous = id
	}
	return total
}

func uvarintLen(v uint64) int {
	n := 1
	for v >= 0x80 {
		v >>= 7
		n++
	}
	return n
}
