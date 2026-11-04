package cas

import (
	"errors"
	"os"
	"path/filepath"
	"testing"
)

func input(t *testing.T, path, content string) Input {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	digest, size, err := HashFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return Input{Path: "src/input.txt", Digest: digest, Bytes: size}
}

func TestActionContextKeyAndExactFetch(t *testing.T) {
	root := t.TempDir()
	store, err := Init(root)
	if err != nil {
		t.Fatal(err)
	}
	in := input(t, filepath.Join(root, "input.txt"), "typed action input\n")
	action := Action{
		ActionType: "compile_object",
		Toolchain:  "rust-1.85.0",
		Inputs:     []Input{in},
	}
	output := []byte("deterministic output bytes\n")
	record, existed, err := store.StoreAction(action, output, "out/object.bin")
	if err != nil {
		t.Fatal(err)
	}
	if existed {
		t.Fatal("first store must be new")
	}
	if record.OutputDigest != BlobDigest(output) {
		t.Fatalf("output digest %s != %s", record.OutputDigest, BlobDigest(output))
	}

	again, existed, err := store.StoreAction(action, output, "out/object.bin")
	if err != nil {
		t.Fatal(err)
	}
	if !existed {
		t.Fatal("second store must be a hit")
	}
	if again.ContextRootID != record.ContextRootID {
		t.Fatalf("same action context produced %s and %s",
			record.ContextRootID, again.ContextRootID)
	}

	fetchDir := filepath.Join(root, "fetch")
	fetched, target, err := store.Fetch(action, fetchDir)
	if err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(target)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != string(output) {
		t.Fatalf("materialized bytes differ: %q", got)
	}
	if fetched.OutputDigest != record.OutputDigest {
		t.Fatal("fetch digest changed")
	}
}

func TestDifferentToolchainAndInputMiss(t *testing.T) {
	root := t.TempDir()
	store, err := Init(root)
	if err != nil {
		t.Fatal(err)
	}
	inA := input(t, filepath.Join(root, "a.txt"), "input A\n")
	inB := input(t, filepath.Join(root, "b.txt"), "input B\n")
	base := Action{ActionType: "compile_object", Toolchain: "rust-1.85.0", Inputs: []Input{inA}}
	if _, _, err := store.StoreAction(base, []byte("out"), "out.bin"); err != nil {
		t.Fatal(err)
	}
	otherToolchain := Action{ActionType: "compile_object", Toolchain: "rust-1.86.0", Inputs: []Input{inA}}
	otherInput := Action{ActionType: "compile_object", Toolchain: "rust-1.85.0", Inputs: []Input{inB}}
	if _, _, err := store.Fetch(otherToolchain, filepath.Join(root, "m1")); !errors.Is(err, ErrMiss) {
		t.Fatalf("different toolchain should miss, got %v", err)
	}
	if _, _, err := store.Fetch(otherInput, filepath.Join(root, "m2")); !errors.Is(err, ErrMiss) {
		t.Fatalf("different input should miss, got %v", err)
	}
}

func TestQueryByContext(t *testing.T) {
	root := t.TempDir()
	store, err := Init(root)
	if err != nil {
		t.Fatal(err)
	}
	in := input(t, filepath.Join(root, "a.txt"), "input A\n")
	action := Action{ActionType: "compile_object", Toolchain: "rust-1.85.0", Inputs: []Input{in}}
	if _, _, err := store.StoreAction(action, []byte("out"), "out.bin"); err != nil {
		t.Fatal(err)
	}
	records, err := store.Query("compile_object", "", "", "")
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 1 {
		t.Fatalf("action-type query returned %d records", len(records))
	}
	records, err = store.Query("", "rust-1.85.0", "", "")
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 1 {
		t.Fatalf("toolchain query returned %d records", len(records))
	}
	records, err = store.Query("", "", "src/input.txt", in.Digest)
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 1 {
		t.Fatalf("input query returned %d records", len(records))
	}
	records, err = store.Query("link", "", "", "")
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 0 {
		t.Fatalf("absent action type returned %d records", len(records))
	}
}

func TestSameKeyDifferentOutputFailsClosed(t *testing.T) {
	root := t.TempDir()
	store, err := Init(root)
	if err != nil {
		t.Fatal(err)
	}
	in := input(t, filepath.Join(root, "a.txt"), "input A\n")
	action := Action{ActionType: "compile_object", Toolchain: "rust-1.85.0", Inputs: []Input{in}}
	if _, _, err := store.StoreAction(action, []byte("first"), "out.bin"); err != nil {
		t.Fatal(err)
	}
	if _, _, err := store.StoreAction(action, []byte("second"), "out.bin"); err == nil {
		t.Fatal("same key with different bytes must fail closed")
	}
}

func TestVerifyReDerivesKeys(t *testing.T) {
	root := t.TempDir()
	store, err := Init(root)
	if err != nil {
		t.Fatal(err)
	}
	in := input(t, filepath.Join(root, "a.txt"), "input A\n")
	action := Action{ActionType: "compile_object", Toolchain: "rust-1.85.0", Inputs: []Input{in}}
	if _, _, err := store.StoreAction(action, []byte("out"), "out.bin"); err != nil {
		t.Fatal(err)
	}
	count, err := store.Verify()
	if err != nil {
		t.Fatal(err)
	}
	if count != 1 {
		t.Fatalf("verify counted %d records", count)
	}
}
