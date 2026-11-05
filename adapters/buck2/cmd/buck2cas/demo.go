package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"strings"
	"time"

	"github.com/infinityabundance/WeftFS-Go/store"
	"github.com/infinityabundance/WeftFS-Go/werr"
	"github.com/infinityabundance/WeftFS/adapters/buck2/cas"
)

type assertion struct {
	Name   string `json:"name"`
	Pass   bool   `json:"pass"`
	Detail string `json:"detail,omitempty"`
}

type mountPlan struct {
	StoreRoot       string `json:"store_root"`
	ContextName     string `json:"context_name"`
	ContextRootID   string `json:"context_root_id"`
	SnapshotID      string `json:"snapshot_id"`
	Selector        string `json:"selector"`
	OutputObjectID  string `json:"output_object_id"`
	OutputStorePath string `json:"output_store_path"`
	OutputBasename  string `json:"output_basename"`
	ExpectedName    string `json:"expected_mount_name"`
	ContentID       string `json:"content_id"`
	SHA256          string `json:"sha256"`
	Bytes           int64  `json:"bytes"`
}

type demoReceipt struct {
	Kind          string            `json:"kind"`
	Schema        string            `json:"schema"`
	TimestampUTC  string            `json:"timestamp_utc"`
	Revision      string            `json:"revision"`
	Revisions     map[string]string `json:"revisions"`
	Environment   map[string]string `json:"environment"`
	Label         string            `json:"label"`
	StoreRoot     string            `json:"store_root"`
	Key           cas.Key           `json:"key"`
	OtherKey      cas.Key           `json:"other_toolchain_key"`
	OtherInputKey cas.Key           `json:"other_input_key"`
	Record        cas.Record        `json:"record"`
	Query         cas.QueryResult   `json:"query"`
	Context       cas.ContextInfo   `json:"context"`
	GC            cas.GCReport      `json:"gc"`
	GCStore       string            `json:"gc_store"`
	PhysicalBytes cas.PhysicalBytes `json:"physical_bytes"`
	Mount         mountPlan         `json:"mount"`
	CrossLanguage map[string]any    `json:"cross_language"`
	Buck2Binary   string            `json:"buck2_binary"`
	Assertions    []assertion       `json:"assertions"`
	Result        map[string]any    `json:"result"`
}

// gcCourt proves that GC collects an unreferenced action while a named
// snapshot plus Context Root retain another action's output.
func gcCourt(root string, opts store.Options, retainedAction cas.Action, retainedOutput []byte, add func(name string, pass bool, detail string)) (cas.GCReport, error) {
	if err := os.RemoveAll(root); err != nil {
		return cas.GCReport{}, err
	}
	s, err := cas.Init(root, opts)
	if err != nil {
		return cas.GCReport{}, err
	}
	defer s.Close()

	retainedRecord, _, err := s.StoreAction(retainedAction, retainedOutput, "out/object.bin")
	if err != nil {
		return cas.GCReport{}, err
	}
	retainedManifest, err := s.ManifestPath(retainedRecord.OutputObjectID)
	if err != nil {
		return cas.GCReport{}, err
	}
	snapshot, err := s.Snapshot("keep")
	if err != nil {
		return cas.GCReport{}, err
	}
	contextInfo, err := s.CreateContext(retainedRecord.ActionKey, "by:action="+retainedRecord.ActionKey, "keep", false)
	if err != nil {
		return cas.GCReport{}, err
	}

	collectedAction := cas.Action{
		ActionType: "gc_unreferenced",
		Toolchain:  "rust-1.85.0",
		Inputs: []cas.Input{{
			Path:      "src/gc.txt",
			ContentID: cas.ContentID([]byte("gc input\n")),
			Bytes:     9,
		}},
	}
	collectedOutput := []byte("unique bytes for the unreferenced action\n")
	collectedRecord, _, err := s.StoreAction(collectedAction, collectedOutput, "out/gc.bin")
	if err != nil {
		return cas.GCReport{}, err
	}
	collectedManifest, err := s.ManifestPath(collectedRecord.OutputObjectID)
	if err != nil {
		return cas.GCReport{}, err
	}

	if err := s.DeleteAction(retainedRecord.ActionKey); err != nil {
		return cas.GCReport{}, err
	}
	if err := s.DeleteAction(collectedRecord.ActionKey); err != nil {
		return cas.GCReport{}, err
	}
	if count, err := s.VerifyAll(); err != nil || count != 0 {
		return cas.GCReport{}, fmt.Errorf("delete left records behind: count=%d err=%v", count, err)
	}
	add("both actions are unreferenced in the live head", true, "records=0")

	report, err := s.GC(true, 0)
	if err != nil {
		return cas.GCReport{}, err
	}
	add("GC ran with the writer lock and applied deletions",
		report.Applied, fmt.Sprintf("%d files/%d bytes", report.DeletedFiles, report.DeletedBytes))
	add("GC deleted the unreferenced action's manifest", !exists(collectedManifest), collectedManifest)
	add("GC retained the snapshot-referenced action's manifest", exists(retainedManifest), retainedManifest)

	afterContext, err := s.ResolveContext(contextInfo.Name, true)
	if err != nil {
		return cas.GCReport{}, err
	}
	add("retained Context Root still resolves exactly one output",
		afterContext.MatchedCount == 1 && len(afterContext.ObjectIDs) == 1,
		fmt.Sprintf("matched=%d", afterContext.MatchedCount))
	retainedBytes, err := s.ReadObjectAt(afterContext.SnapshotID, afterContext.ObjectIDs[0])
	add("retained Context Root still reads exact bytes after GC",
		err == nil && bytes.Equal(retainedBytes, retainedOutput), fmt.Sprintf("%d bytes, err=%v", len(retainedBytes), err))
	add("the live head moved past the pinned snapshot",
		s.Head() != snapshot, s.Head()+" != "+snapshot)
	add("GC kept the Context Root pinned to the same snapshot",
		afterContext.SnapshotID == snapshot, afterContext.SnapshotID)
	if _, _, err := s.Fetch(collectedRecord.ActionKey, ""); !errors.Is(err, cas.ErrMiss) {
		return cas.GCReport{}, fmt.Errorf("collected action still fetches: %v", err)
	}
	add("the collected action no longer fetches", true, "ErrMiss")
	return report, nil
}

func cmdDemoStore(args []string) error {
	fs := flagSet("demo-store")
	root := fs.String("root", "", "WeftFS store root to create")
	receiptPath := fs.String("receipt", "", "receipt JSON path")
	rustBin := fs.String("rust-bin", os.Getenv("WEFTFS_RUST_BIN"), "Rust weftfs oracle binary")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" || *receiptPath == "" {
		return errors.New("demo-store requires --root/--receipt")
	}

	if err := os.RemoveAll(*root); err != nil {
		return err
	}
	work := *root + ".work"
	if err := os.RemoveAll(work); err != nil {
		return err
	}
	if err := os.MkdirAll(work, 0o755); err != nil {
		return err
	}

	var assertions []assertion
	add := func(name string, pass bool, detail string) {
		assertions = append(assertions, assertion{Name: name, Pass: pass, Detail: detail})
	}

	inputA := []byte("typed action input A\n")
	inputB := []byte("typed action input B\n")
	if err := os.WriteFile(filepath.Join(work, "input-a.txt"), inputA, 0o644); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(work, "input-b.txt"), inputB, 0o644); err != nil {
		return err
	}
	outputBytes := []byte("deterministic object bytes for the action context\n")
	outputPath := "out/object.bin"

	actionFor := func(toolchain string, input []byte) cas.Action {
		return cas.Action{
			ActionType: "compile_object",
			Toolchain:  toolchain,
			Inputs: []cas.Input{{
				Path:      "src/input.txt",
				ContentID: cas.ContentID(input),
				Bytes:     int64(len(input)),
			}},
			Params: map[string]string{"opt": "2", "target": "x86_64"},
		}
	}
	a1 := actionFor("rust-1.85.0", inputA)
	a3 := actionFor("rust-1.86.0", inputA)
	a4 := actionFor("rust-1.85.0", inputB)
	key1, err := cas.DeriveKey(a1)
	if err != nil {
		return err
	}
	key2, err := cas.DeriveKey(a1)
	if err != nil {
		return err
	}
	key3, err := cas.DeriveKey(a3)
	if err != nil {
		return err
	}
	key4, err := cas.DeriveKey(a4)
	if err != nil {
		return err
	}
	add("same action declaration -> same action key",
		key1.ActionKey == key2.ActionKey, key1.ActionKey+" == "+key2.ActionKey)
	add("different toolchain -> different action key",
		key1.ActionKey != key3.ActionKey, key1.ActionKey+" != "+key3.ActionKey)
	add("different input content -> different action key",
		key1.ActionKey != key4.ActionKey, key1.ActionKey+" != "+key4.ActionKey)
	add("action key is a 32-byte BLAKE3 digest (64 hex)",
		len(key1.ActionKey) == 64, fmt.Sprintf("%d hex chars", len(key1.ActionKey)))

	opts := store.DefaultOptions()
	opts.GCGraceSecs = 0
	opts.CheckpointInterval = 1_000_000
	fixed := int64(1767225600000000000) // 2026-01-01T00:00:00Z
	opts.FixedTimestampNS = &fixed
	s, err := cas.Init(*root, opts)
	if err != nil {
		return err
	}

	record1, existed, err := s.StoreAction(a1, outputBytes, outputPath)
	if err != nil {
		s.Close()
		return err
	}
	add("first store publishes a new record", !existed, fmt.Sprintf("existed=%t", existed))
	add("the stored blob ContentId is the output content root",
		record1.OutputContentID == cas.ContentID(outputBytes),
		record1.OutputContentID+" == "+cas.ContentID(outputBytes))

	again, existed, err := s.StoreAction(a1, outputBytes, outputPath)
	if err != nil {
		s.Close()
		return err
	}
	add("second store is an idempotent hit", existed, fmt.Sprintf("existed=%t", existed))
	add("same key returns the existing record",
		again.OutputObjectID == record1.OutputObjectID && again.OutputContentID == record1.OutputContentID,
		again.OutputObjectID)

	fetched, data, err := s.Fetch(key1.ActionKey, filepath.Join(work, "fetched.bin"))
	if err != nil {
		s.Close()
		return err
	}
	add("fetch materializes exact bytes",
		bytes.Equal(data, outputBytes), fmt.Sprintf("%d bytes", len(data)))
	add("fetch digest matches the record", fetched.OutputContentID == record1.OutputContentID,
		fetched.OutputContentID)

	query, err := s.QueryByAction(key1.ActionKey)
	if err != nil {
		s.Close()
		return err
	}
	add("query --by action=<key> resolves exactly one output",
		query.OutputObjectID == record1.OutputObjectID,
		fmt.Sprintf("content_id=%s path=%s", query.ContentID, query.StorePath))
	add("query content id equals the stored output root",
		query.ContentID == record1.OutputContentID, query.ContentID)

	if _, _, err := s.Fetch(key3.ActionKey, ""); !errors.Is(err, cas.ErrMiss) {
		s.Close()
		return fmt.Errorf("different toolchain should miss, got %v", err)
	}
	add("different toolchain misses", true, "ErrMiss")
	if _, _, err := s.Fetch(key4.ActionKey, ""); !errors.Is(err, cas.ErrMiss) {
		s.Close()
		return fmt.Errorf("different input should miss, got %v", err)
	}
	add("different input misses", true, "ErrMiss")

	headBefore := s.Head()
	_, _, err = s.StoreAction(a1, []byte("different output bytes\n"), outputPath)
	conflict := false
	var typed *werr.Error
	if errors.As(err, &typed) && typed.Kind == werr.Conflict {
		conflict = true
	}
	add("same key with different bytes fails closed", conflict, fmt.Sprint(err))
	add("conflict publishes nothing (head unchanged)", s.Head() == headBefore,
		headBefore+" == "+s.Head())
	if count, err := s.VerifyAll(); err != nil || count != 1 {
		s.Close()
		return fmt.Errorf("conflict changed the record set: count=%d err=%v", count, err)
	}
	add("conflict leaves exactly one action record", true, "count=1")

	// A named snapshot is a GC root. The Context Root below pins it.
	if _, err := s.Snapshot("keep"); err != nil {
		s.Close()
		return err
	}
	contextInfo, err := s.CreateContext(key1.ActionKey, "by:action="+key1.ActionKey, "keep", false)
	if err != nil {
		s.Close()
		return err
	}
	contextValue, err := s.ResolveContext(contextInfo.Name, true)
	if err != nil {
		s.Close()
		return err
	}
	add("context by:action=<key> resolves exactly one object",
		contextValue.MatchedCount == 1, fmt.Sprintf("matched=%d", contextValue.MatchedCount))
	contextBytes, err := s.ReadObjectAt(contextValue.SnapshotID, contextValue.ObjectIDs[0])
	add("context resolves the action output content id",
		err == nil && cas.ContentID(contextBytes) == record1.OutputContentID,
		fmt.Sprintf("content_id=%s err=%v", cas.ContentID(contextBytes), err))

	// The tamper test needs the output manifest path; the GC court below uses a
	// separate store so this primary store stays mountable and fetchable.
	a1Manifest, err := s.ManifestPath(record1.OutputObjectID)
	if err != nil {
		s.Close()
		return err
	}

	// Tamper detection: flip one manifest byte, verify must fail, restore.
	original, err := os.ReadFile(a1Manifest)
	if err != nil {
		s.Close()
		return err
	}
	tampered := append([]byte(nil), original...)
	tampered[len(tampered)/2] ^= 0xff
	if err := os.WriteFile(a1Manifest, tampered, 0o644); err != nil {
		s.Close()
		return err
	}
	_, tamperErr := s.Verify(key1.ActionKey, nil)
	add("verify detects a tampered manifest", tamperErr != nil, fmt.Sprint(tamperErr))
	if err := os.WriteFile(a1Manifest, original, 0o644); err != nil {
		s.Close()
		return err
	}
	if _, err := s.Verify(key1.ActionKey, outputBytes); err != nil {
		s.Close()
		return err
	}
	add("verify passes again after restoring the manifest", true, "content id re-derived")

	// Cross-language: Rust reads the Go-written store, and Go reads a store
	// written by the Rust CLI. Skipped honestly when the oracle is absent.
	crossLanguage := map[string]any{"rust_oracle": *rustBin}
	if *rustBin != "" && executable(*rustBin) {
		crossLanguage["rust_oracle_present"] = true
		rustVerify, err := runRust(*rustBin, "--store", *root, "verify", "--full")
		add("[cross] Rust verify --full accepts the Go-written store", err == nil, firstLine(rustVerify, err))
		rustBytes, err := runRustBytes(*rustBin, "--store", *root, "cat", query.StorePath)
		add("[cross] Rust cat returns the exact CAS bytes", err == nil && bytes.Equal(rustBytes, outputBytes),
			fmt.Sprintf("%d bytes, err=%v", len(rustBytes), err))
		rustStat, err := runRust(*rustBin, "--store", *root, "--json", "stat", query.StorePath)
		contentHash := ""
		if err == nil {
			var stat struct {
				ContentHash string `json:"content_hash"`
			}
			if jsonErr := json.Unmarshal([]byte(rustStat), &stat); jsonErr == nil {
				contentHash = stat.ContentHash
			}
		}
		add("[cross] Rust stat reports the same ContentId",
			err == nil && contentHash == record1.OutputContentID, contentHash)
		rustContext, err := runRust(*rustBin, "--store", *root, "--json", "context", "resolve", contextInfo.Name)
		add("[cross] Rust resolves the Go-written Context Root", err == nil && strings.Contains(rustContext, `"matched_count": 1`),
			firstLine(rustContext, err))
		crossLanguage["go_written_store_rust_verified"] = err == nil

		if err := crossLanguageReverse(*rustBin, *root+"-rust", work, fixed, &assertions); err != nil {
			crossLanguage["rust_written_store_error"] = err.Error()
		} else {
			crossLanguage["rust_written_store_go_verified"] = true
		}
	} else {
		crossLanguage["rust_oracle_present"] = false
		crossLanguage["note"] = "Rust oracle binary absent; cross-language checks were not run in this court"
	}

	buck2Binary := "absent"
	if _, err := exec.LookPath("buck2"); err == nil {
		buck2Binary = "present"
		if output, probeErr := exec.Command("buck2", "--version").CombinedOutput(); probeErr == nil {
			buck2Binary = strings.TrimSpace(string(output))
		}
	}
	crossLanguage["buck2_binary"] = buck2Binary
	add("Buck2 itself was not used as a plugin or worker", true, "buck2 binary: "+buck2Binary)

	// GC evidence runs on a dedicated store: one action is pinned by a named
	// snapshot and retained Context Root, a later action is unreferenced.
	gcReport, err := gcCourt(*root+"-gc", opts, a1, outputBytes, add)
	if err != nil {
		s.Close()
		return err
	}

	physical, err := s.PhysicalBytes()
	if err != nil {
		s.Close()
		return err
	}
	head := s.Head()
	generation := s.Generation()
	if err := s.Close(); err != nil {
		return err
	}

	sum := sha256.Sum256(outputBytes)
	base := path.Base(record1.OutputPath)
	maxName := len(record1.OutputObjectID)
	if maxName > 8 {
		maxName = 8
	}
	mount := mountPlan{
		StoreRoot:       *root,
		ContextName:     contextInfo.Name,
		ContextRootID:   contextInfo.ContextRootID,
		SnapshotID:      contextInfo.SnapshotID,
		Selector:        contextInfo.Selector,
		OutputObjectID:  record1.OutputObjectID,
		OutputStorePath: query.StorePath,
		OutputBasename:  base,
		ExpectedName:    base + "--" + record1.OutputObjectID[:maxName],
		ContentID:       record1.OutputContentID,
		SHA256:          hex.EncodeToString(sum[:]),
		Bytes:           int64(len(outputBytes)),
	}

	passed := 0
	for _, item := range assertions {
		if item.Pass {
			passed++
		}
	}
	revisions := map[string]string{
		"WeftFS":       os.Getenv("WEFT_REPO_WEFTFS"),
		"WeftCodec":    os.Getenv("WEFT_REPO_WEFTCODEC"),
		"WeftCodec-Go": os.Getenv("WEFT_REPO_WEFTCODEC_GO"),
		"WeftFS-Go":    os.Getenv("WEFT_REPO_WEFTFS_GO"),
	}
	environment := map[string]string{
		"court_image":    os.Getenv("WEFT_COURT_IMAGE"),
		"court_image_id": os.Getenv("WEFT_COURT_IMAGE_ID"),
		"kernel":         os.Getenv("WEFT_COURT_KERNEL"),
		"memory":         os.Getenv("WEFT_COURT_MEMORY"),
		"cpus":           os.Getenv("WEFT_COURT_CPUS"),
		"pids":           os.Getenv("WEFT_COURT_PIDS"),
		"nproc":          os.Getenv("WEFT_COURT_NPROC"),
		"docker_server":  os.Getenv("WEFT_DOCKER_SERVER"),
	}
	receipt := demoReceipt{
		Kind:          "buck2-cas-store",
		Schema:        "weft.buck2-cas-store.receipt/1",
		TimestampUTC:  time.Now().UTC().Format(time.RFC3339),
		Revision:      os.Getenv("WEFT_REPO_WEFTFS"),
		Revisions:     revisions,
		Environment:   environment,
		Label:         "single host, synthetic deterministic fixtures, Docker court, local WeftFS store; not a fleet or Buck2 claim",
		StoreRoot:     *root,
		Key:           key1,
		OtherKey:      key3,
		OtherInputKey: key4,
		Record:        record1,
		Query:         query,
		Context:       contextInfo,
		GC:            gcReport,
		GCStore:       *root + "-gc",
		PhysicalBytes: physical,
		Mount:         mount,
		CrossLanguage: crossLanguage,
		Buck2Binary:   buck2Binary,
		Assertions:    assertions,
		Result: map[string]any{
			"pass":       passed == len(assertions),
			"passed":     passed,
			"total":      len(assertions),
			"generation": generation,
			"head":       head,
		},
	}
	if err := writeJSONFile(*receiptPath, receipt); err != nil {
		return err
	}
	// A small KEY=VALUE file lets the shell round-trip the mount plan without
	// depending on jq in the FUSE court.
	env := fmt.Sprintf(
		"ACTION_KEY=%s\nSTORE=%s\nCONTEXT_NAME=%s\nCONTEXT_ROOT_ID=%s\nSNAPSHOT_ID=%s\nOUTPUT_OBJECT_ID=%s\nOUTPUT_STORE_PATH=%s\nEXPECTED_NAME=%s\nCONTENT_ID=%s\nOUTPUT_SHA256=%s\nOUTPUT_BYTES=%d\n",
		key1.ActionKey, mount.StoreRoot, mount.ContextName, mount.ContextRootID, mount.SnapshotID,
		mount.OutputObjectID, mount.OutputStorePath, mount.ExpectedName,
		mount.ContentID, mount.SHA256, mount.Bytes)
	if err := os.WriteFile(*receiptPath+".mount.env", []byte(env), 0o644); err != nil {
		return err
	}
	fmt.Printf("buck2-cas store demo: %d/%d assertions pass; receipt %s\n",
		passed, len(assertions), *receiptPath)
	if passed != len(assertions) {
		return errors.New("demo assertions failed")
	}
	return nil
}

// crossLanguageReverse writes a store entirely through the Rust CLI and proves
// the Go adapter reads and verifies it.
func crossLanguageReverse(rustBin, root, work string, fixed int64, assertions *[]assertion) error {
	add := func(name string, pass bool, detail string) {
		*assertions = append(*assertions, assertion{Name: name, Pass: pass, Detail: detail})
	}
	if err := os.RemoveAll(root); err != nil {
		return err
	}
	if _, err := runRust(rustBin, "--store", root, "--fixed-timestamp-ns", fmt.Sprint(fixed), "init"); err != nil {
		return err
	}
	output := []byte("rust-written CAS output bytes\n")
	input := []byte("rust-written action input\n")
	action := cas.Action{
		ActionType: "link_object",
		Toolchain:  "rust-1.85.0",
		Inputs: []cas.Input{{
			Path:      "src/lib.rs",
			ContentID: cas.ContentID(input),
			Bytes:     int64(len(input)),
		}},
		Params: map[string]string{"linker": "ld"},
	}
	key, err := cas.DeriveKey(action)
	if err != nil {
		return err
	}
	record, err := cas.NewRecord(key, action, "bin/app", output, key.OutputObjectID)
	if err != nil {
		return err
	}
	encoded, err := cas.RecordBytes(record)
	if err != nil {
		return err
	}
	blobFile := filepath.Join(work, "rust-output.bin")
	recordFile := filepath.Join(work, "rust-record.wcb")
	if err := os.WriteFile(blobFile, output, 0o644); err != nil {
		return err
	}
	if err := os.WriteFile(recordFile, encoded, 0o644); err != nil {
		return err
	}
	outPath := "/cas/outputs/" + key.ActionKey + "/bin/app"
	dirs := []string{"/cas", "/cas/records", "/cas/outputs", "/cas/outputs/" + key.ActionKey, path.Dir(outPath)}
	seenDir := map[string]bool{}
	for _, dir := range dirs {
		if seenDir[dir] {
			continue
		}
		seenDir[dir] = true
		if _, err := runRust(rustBin, "--store", root, "mkdir", dir, "--seed", cas.DirSeedPrefix+dir); err != nil {
			return err
		}
	}
	if _, err := runRust(rustBin, "--store", root, "--fixed-timestamp-ns", fmt.Sprint(fixed),
		"create", outPath, "--file", blobFile, "--seed", cas.OutputSeedPrefix+key.ActionKey); err != nil {
		return err
	}
	if _, err := runRust(rustBin, "--store", root, "--fixed-timestamp-ns", fmt.Sprint(fixed),
		"create", "/cas/records/"+key.ActionKey, "--file", recordFile,
		"--seed", cas.RecordSeedPrefix+key.ActionKey); err != nil {
		return err
	}
	recordObjectID := cas.ObjectIDForSeed(cas.RecordSeedPrefix + key.ActionKey).Hex()
	outputObjectID := cas.ObjectIDForSeed(cas.OutputSeedPrefix + key.ActionKey).Hex()
	if _, err := runRust(rustBin, "--store", root, "relation", "add",
		"--subject", recordObjectID, "--predicate", cas.PredicateAction,
		"--object", outputObjectID); err != nil {
		return err
	}
	if _, err := runRust(rustBin, "--store", root, "relation", "add",
		"--subject", outputObjectID, "--predicate", cas.PredicateOutputOf,
		"--object", recordObjectID); err != nil {
		return err
	}
	s, err := cas.OpenReadOnly(root)
	if err != nil {
		return err
	}
	defer s.Close()
	if _, err := runRust(rustBin, "--store", root, "verify", "--full"); err != nil {
		return err
	}
	record2, fetched, err := s.Fetch(key.ActionKey, filepath.Join(work, "rust-fetched.bin"))
	add("[cross] Go fetch reads a Rust-written action record",
		err == nil && bytes.Equal(fetched, output), fmt.Sprintf("%d bytes, err=%v", len(fetched), err))
	add("[cross] Go verify accepts a Rust-written action", err == nil && record2.OutputContentID == cas.ContentID(output),
		record2.OutputContentID)
	query, err := s.QueryByAction(key.ActionKey)
	add("[cross] Go query resolves the Rust-written action", err == nil && query.ContentID == cas.ContentID(output),
		fmt.Sprintf("content_id=%s err=%v", query.ContentID, err))
	if err != nil {
		return err
	}
	return nil
}

func runRust(bin string, args ...string) (string, error) {
	cmd := exec.Command(bin, args...)
	cmd.Env = append(os.Environ(), "NO_COLOR=1")
	output, err := cmd.CombinedOutput()
	if err != nil {
		return string(output), fmt.Errorf("rust %s failed: %v\n%s", strings.Join(args, " "), err, output)
	}
	return string(output), nil
}

func runRustBytes(bin string, args ...string) ([]byte, error) {
	cmd := exec.Command(bin, args...)
	cmd.Env = append(os.Environ(), "NO_COLOR=1")
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	output, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("rust %s failed: %v\n%s", strings.Join(args, " "), err, stderr.String())
	}
	return output, nil
}

func executable(file string) bool {
	info, err := os.Stat(file)
	return err == nil && !info.IsDir() && info.Mode()&0o111 != 0
}

func exists(file string) bool {
	_, err := os.Stat(file)
	return err == nil
}

func firstLine(text string, err error) string {
	first := strings.SplitN(strings.TrimSpace(text), "\n", 2)[0]
	if err != nil {
		return fmt.Sprintf("%v | %s", err, first)
	}
	return first
}

func writeJSONFile(target string, value any) error {
	if err := os.MkdirAll(filepath.Dir(target), 0o755); err != nil {
		return err
	}
	encoded, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(target, append(encoded, '\n'), 0o644)
}
