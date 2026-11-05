package cas

import (
	"bytes"
	"errors"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"strings"
	"testing"

	"github.com/infinityabundance/WeftFS-Go/store"
	"github.com/infinityabundance/WeftFS-Go/werr"
)

func testOptions() store.Options {
	opts := store.DefaultOptions()
	opts.GCGraceSecs = 0
	opts.CheckpointInterval = 1_000_000
	fixed := int64(1767225600000000000)
	opts.FixedTimestampNS = &fixed
	return opts
}

func newStore(t *testing.T) (*Store, string) {
	t.Helper()
	root := t.TempDir()
	s, err := Init(root, testOptions())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = s.Close() })
	return s, root
}

func action(toolchain string, input []byte) Action {
	return Action{
		ActionType: "compile_object",
		Toolchain:  toolchain,
		Inputs: []Input{{
			Path:      "src/input.txt",
			ContentID: ContentID(input),
			Bytes:     int64(len(input)),
		}},
		Params: map[string]string{"opt": "2"},
	}
}

func TestKeyDerivationIsCanonical(t *testing.T) {
	inputA := []byte("input A\n")
	inputB := []byte("input B\n")
	a := Action{
		ActionType: "compile_object",
		Toolchain:  "rust-1.85.0",
		Inputs: []Input{
			{Path: "b.txt", ContentID: ContentID(inputB), Bytes: int64(len(inputB))},
			{Path: "a.txt", ContentID: ContentID(inputA), Bytes: int64(len(inputA))},
		},
		Params: map[string]string{"z": "1", "a": "2"},
	}
	reordered := Action{
		ActionType: "compile_object",
		Toolchain:  "rust-1.85.0",
		Inputs: []Input{
			{Path: "a.txt", ContentID: ContentID(inputA), Bytes: int64(len(inputA))},
			{Path: "b.txt", ContentID: ContentID(inputB), Bytes: int64(len(inputB))},
		},
		Params: map[string]string{"a": "2", "z": "1"},
	}
	key1, err := DeriveKey(a)
	if err != nil {
		t.Fatal(err)
	}
	key2, err := DeriveKey(reordered)
	if err != nil {
		t.Fatal(err)
	}
	if key1.ActionKey != key2.ActionKey {
		t.Fatalf("declaration order changed the key: %s != %s", key1.ActionKey, key2.ActionKey)
	}
	other, _ := DeriveKey(action("rust-1.86.0", inputA))
	if other.ActionKey == key1.ActionKey {
		t.Fatal("toolchain must be key material")
	}
	otherParams := a
	otherParams.Params = map[string]string{"opt": "3"}
	otherKey, _ := DeriveKey(otherParams)
	if otherKey.ActionKey == key1.ActionKey {
		t.Fatal("params must be key material")
	}
	if len(key1.ActionKey) != 64 {
		t.Fatalf("action key is %d hex chars", len(key1.ActionKey))
	}
}

func TestStoreIdempotentFetchQueryAndContext(t *testing.T) {
	s, _ := newStore(t)
	input := []byte("input A\n")
	a := action("rust-1.85.0", input)
	output := []byte("deterministic bytes\n")
	record, existed, err := s.StoreAction(a, output, "out/object.bin")
	if err != nil {
		t.Fatal(err)
	}
	if existed {
		t.Fatal("first store must be new")
	}
	if record.OutputContentID != ContentID(output) {
		t.Fatalf("record content id %s != %s", record.OutputContentID, ContentID(output))
	}
	again, existed, err := s.StoreAction(a, output, "out/object.bin")
	if err != nil {
		t.Fatal(err)
	}
	if !existed || again.OutputObjectID != record.OutputObjectID {
		t.Fatal("second store must be an idempotent hit")
	}
	_, fetched, err := s.Fetch(record.ActionKey, "")
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(fetched, output) {
		t.Fatal("fetch returned different bytes")
	}
	query, err := s.QueryByAction(record.ActionKey)
	if err != nil {
		t.Fatal(err)
	}
	if query.ContentID != record.OutputContentID || query.OutputObjectID != record.OutputObjectID {
		t.Fatalf("query resolved %+v", query)
	}
	if _, _, err := s.Fetch(mustKey(t, action("rust-1.85.0", []byte("other\n"))), ""); !errors.Is(err, ErrMiss) {
		t.Fatalf("different input should miss, got %v", err)
	}
	if _, err := s.Snapshot("keep"); err != nil {
		t.Fatal(err)
	}
	info, err := s.CreateContext(record.ActionKey, "by:action="+record.ActionKey, "keep", false)
	if err != nil {
		t.Fatal(err)
	}
	value, err := s.ResolveContext(info.Name, true)
	if err != nil {
		t.Fatal(err)
	}
	if value.MatchedCount != 1 || len(value.ObjectIDs) != 1 || value.ObjectIDs[0] != record.OutputObjectID {
		t.Fatalf("context resolved %+v", value)
	}
}

func mustKey(t *testing.T, a Action) string {
	t.Helper()
	key, err := DeriveKey(a)
	if err != nil {
		t.Fatal(err)
	}
	return key.ActionKey
}

func TestConflictFailsClosedWithoutMutation(t *testing.T) {
	s, _ := newStore(t)
	a := action("rust-1.85.0", []byte("input A\n"))
	first := []byte("first output\n")
	record, _, err := s.StoreAction(a, first, "out/object.bin")
	if err != nil {
		t.Fatal(err)
	}
	headBefore := s.Head()
	physicalBefore, err := s.PhysicalBytes()
	if err != nil {
		t.Fatal(err)
	}
	_, _, err = s.StoreAction(a, []byte("second output\n"), "out/object.bin")
	var typed *werr.Error
	if !errors.As(err, &typed) || typed.Kind != werr.Conflict {
		t.Fatalf("expected Conflict, got %v", err)
	}
	if s.Head() != headBefore {
		t.Fatal("conflict published a commit")
	}
	physicalAfter, err := s.PhysicalBytes()
	if err != nil {
		t.Fatal(err)
	}
	if physicalAfter.Total != physicalBefore.Total || physicalAfter.Files != physicalBefore.Files {
		t.Fatalf("conflict wrote store files: %+v -> %+v", physicalBefore, physicalAfter)
	}
	if count, err := s.VerifyAll(); err != nil || count != 1 {
		t.Fatalf("conflict changed the record set: count=%d err=%v", count, err)
	}
	_, fetched, err := s.Fetch(record.ActionKey, "")
	if err != nil || !bytes.Equal(fetched, first) {
		t.Fatalf("original output changed: %v", err)
	}
	// The rejected blob must not have become a reachable CAS object: the
	// original bytes are still the only fetchable output.
	if _, _, err := s.Fetch(record.ActionKey, ""); err != nil {
		t.Fatal(err)
	}
}

func TestVerifyDetectsTamperedManifest(t *testing.T) {
	s, _ := newStore(t)
	a := action("rust-1.85.0", []byte("input A\n"))
	output := []byte("tamper target\n")
	record, _, err := s.StoreAction(a, output, "out/object.bin")
	if err != nil {
		t.Fatal(err)
	}
	manifest, err := s.ManifestPath(record.OutputObjectID)
	if err != nil {
		t.Fatal(err)
	}
	original, err := os.ReadFile(manifest)
	if err != nil {
		t.Fatal(err)
	}
	tampered := append([]byte(nil), original...)
	tampered[len(tampered)/2] ^= 0xff
	if err := os.WriteFile(manifest, tampered, 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Verify(record.ActionKey, nil); err == nil {
		t.Fatal("verify accepted a tampered manifest")
	}
	if _, _, err := s.Fetch(record.ActionKey, ""); err == nil {
		t.Fatal("fetch accepted a tampered manifest")
	}
	if err := os.WriteFile(manifest, original, 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Verify(record.ActionKey, output); err != nil {
		t.Fatalf("verify failed after restore: %v", err)
	}
	if _, err := s.Verify(record.ActionKey, []byte("wrong blob\n")); err == nil {
		t.Fatal("verify accepted a different blob")
	}
}

func TestGCKeepsSnapshotAndCollectsUnreferenced(t *testing.T) {
	s, _ := newStore(t)
	a := action("rust-1.85.0", []byte("input A\n"))
	output := []byte("retained output bytes\n")
	record, _, err := s.StoreAction(a, output, "out/object.bin")
	if err != nil {
		t.Fatal(err)
	}
	retainedManifest, err := s.ManifestPath(record.OutputObjectID)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.Snapshot("keep"); err != nil {
		t.Fatal(err)
	}
	info, err := s.CreateContext(record.ActionKey, "by:action="+record.ActionKey, "keep", false)
	if err != nil {
		t.Fatal(err)
	}

	collectedAction := action("rust-1.85.0", []byte("unreferenced input\n"))
	collectedRecord, _, err := s.StoreAction(collectedAction, []byte("unreferenced output bytes\n"), "out/gc.bin")
	if err != nil {
		t.Fatal(err)
	}
	collectedManifest, err := s.ManifestPath(collectedRecord.OutputObjectID)
	if err != nil {
		t.Fatal(err)
	}

	if err := s.DeleteAction(record.ActionKey); err != nil {
		t.Fatal(err)
	}
	if err := s.DeleteAction(collectedRecord.ActionKey); err != nil {
		t.Fatal(err)
	}
	if _, err := s.GC(true, 0); err != nil {
		t.Fatal(err)
	}
	if storePathExists(collectedManifest) {
		t.Fatalf("GC kept the unreferenced manifest %s", collectedManifest)
	}
	if !storePathExists(retainedManifest) {
		t.Fatalf("GC deleted the snapshot-retained manifest %s", retainedManifest)
	}
	value, err := s.ResolveContext(info.Name, true)
	if err != nil {
		t.Fatalf("retained context stopped resolving: %v", err)
	}
	if value.MatchedCount != 1 || len(value.ObjectIDs) != 1 {
		t.Fatalf("retained context result %+v", value)
	}
	data, err := s.ReadObjectAt(value.SnapshotID, value.ObjectIDs[0])
	if err != nil {
		t.Fatalf("retained context bytes unreadable: %v", err)
	}
	if !bytes.Equal(data, output) {
		t.Fatal("retained context bytes changed")
	}
	if _, _, err := s.Fetch(record.ActionKey, ""); !errors.Is(err, ErrMiss) {
		t.Fatal("deleted action still fetches from the live head")
	}
}

func storePathExists(file string) bool {
	_, err := os.Stat(file)
	return err == nil
}

// ---- cross-language -------------------------------------------------------

func rustBinary(t *testing.T) string {
	t.Helper()
	candidates := []string{
		os.Getenv("WEFTFS_RUST_BIN"),
		filepath.Join("..", "..", "..", ".docker-cache", "target-rust", "release", "weftfs"),
		filepath.Join("/work", ".docker-cache", "target-rust", "release", "weftfs"),
	}
	for _, candidate := range candidates {
		if candidate == "" {
			continue
		}
		info, err := os.Stat(candidate)
		if err != nil || info.IsDir() {
			continue
		}
		probe := exec.Command(candidate, "--version")
		if output, probeErr := probe.CombinedOutput(); probeErr != nil {
			t.Skipf("Rust oracle at %s is not runnable in this court: %v\n%s", candidate, probeErr, output)
		}
		return candidate
	}
	t.Skip("Rust oracle binary not found; build it in the fuse court")
	return ""
}

func runRustT(t *testing.T, bin string, args ...string) string {
	t.Helper()
	cmd := exec.Command(bin, args...)
	cmd.Env = append(os.Environ(), "NO_COLOR=1")
	output, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("rust %s failed: %v\n%s", strings.Join(args, " "), err, output)
	}
	return string(output)
}

func TestCrossLanguageGoWriteRustRead(t *testing.T) {
	bin := rustBinary(t)
	s, root := newStore(t)
	a := action("rust-1.85.0", []byte("cross input\n"))
	output := []byte("cross-language output bytes\n")
	record, _, err := s.StoreAction(a, output, "out/object.bin")
	if err != nil {
		t.Fatal(err)
	}
	storePath := OutputPath(record.ActionKey, record.OutputPath)
	if _, err := s.Snapshot("keep"); err != nil {
		t.Fatal(err)
	}
	info, err := s.CreateContext(record.ActionKey, "by:action="+record.ActionKey, "keep", false)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	runRustT(t, bin, "--store", root, "verify", "--full")
	stat := runRustT(t, bin, "--store", root, "--json", "stat", storePath)
	if !strings.Contains(stat, `"content_hash": "`+record.OutputContentID+`"`) {
		t.Fatalf("Rust stat did not report the CAS ContentId:\n%s", stat)
	}
	cmd := exec.Command(bin, "--store", root, "cat", storePath)
	cmd.Env = append(os.Environ(), "NO_COLOR=1")
	got, err := cmd.Output()
	if err != nil {
		t.Fatalf("rust cat: %v", err)
	}
	if !bytes.Equal(got, output) {
		t.Fatal("Rust cat returned different bytes")
	}
	resolved := runRustT(t, bin, "--store", root, "--json", "context", "resolve", info.Name)
	if !strings.Contains(resolved, `"matched_count": 1`) {
		t.Fatalf("Rust could not resolve the Go-written context:\n%s", resolved)
	}
}

func TestCrossLanguageRustWriteGoRead(t *testing.T) {
	bin := rustBinary(t)
	root := filepath.Join(t.TempDir(), "rust-store")
	if err := os.MkdirAll(root, 0o755); err != nil {
		t.Fatal(err)
	}
	runRustT(t, bin, "--store", root, "init")
	a := action("rust-1.85.0", []byte("rust-written input\n"))
	output := []byte("rust-written output bytes\n")
	key, err := DeriveKey(a)
	if err != nil {
		t.Fatal(err)
	}
	record, err := NewRecord(key, a, "out/object.bin", output, key.OutputObjectID)
	if err != nil {
		t.Fatal(err)
	}
	encoded, err := RecordBytes(record)
	if err != nil {
		t.Fatal(err)
	}
	work := t.TempDir()
	blobFile := filepath.Join(work, "blob.bin")
	recordFile := filepath.Join(work, "record.wcb")
	if err := os.WriteFile(blobFile, output, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(recordFile, encoded, 0o644); err != nil {
		t.Fatal(err)
	}
	storePath := "/cas/outputs/" + key.ActionKey + "/out/object.bin"
	dirs := []string{"/cas", "/cas/records", "/cas/outputs", "/cas/outputs/" + key.ActionKey, path.Dir(storePath)}
	seenDir := map[string]bool{}
	for _, dir := range dirs {
		if seenDir[dir] {
			continue
		}
		seenDir[dir] = true
		runRustT(t, bin, "--store", root, "mkdir", dir, "--seed", DirSeedPrefix+dir)
	}
	runRustT(t, bin, "--store", root, "create", storePath, "--file", blobFile, "--seed", OutputSeedPrefix+key.ActionKey)
	runRustT(t, bin, "--store", root, "create", "/cas/records/"+key.ActionKey, "--file", recordFile, "--seed", RecordSeedPrefix+key.ActionKey)
	recordID := ObjectIDForSeed(RecordSeedPrefix + key.ActionKey).Hex()
	outputID := ObjectIDForSeed(OutputSeedPrefix + key.ActionKey).Hex()
	runRustT(t, bin, "--store", root, "relation", "add", "--subject", recordID, "--predicate", PredicateAction, "--object", outputID)
	runRustT(t, bin, "--store", root, "relation", "add", "--subject", outputID, "--predicate", PredicateOutputOf, "--object", recordID)
	runRustT(t, bin, "--store", root, "verify", "--full")

	s, err := OpenReadOnly(root)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	fetchedRecord, fetched, err := s.Fetch(key.ActionKey, "")
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(fetched, output) {
		t.Fatal("Go fetch returned different bytes for a Rust-written store")
	}
	if _, err := s.Verify(key.ActionKey, output); err != nil {
		t.Fatalf("Go verify rejected a Rust-written action: %v", err)
	}
	query, err := s.QueryByAction(key.ActionKey)
	if err != nil {
		t.Fatal(err)
	}
	if query.ContentID != fetchedRecord.OutputContentID {
		t.Fatalf("Go query resolved %+v", query)
	}
}
