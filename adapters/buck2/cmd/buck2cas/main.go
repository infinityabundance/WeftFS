// Command buck2cas is a prototype local action-cache adapter keyed by a typed
// Weft Context Root. It is not a Buck2 plugin and does not speak REAPI gRPC.
package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/infinityabundance/WeftFS/adapters/buck2/cas"
)

func main() {
	if len(os.Args) < 2 {
		usage()
		os.Exit(2)
	}
	var err error
	switch os.Args[1] {
	case "init":
		err = cmdInit(os.Args[2:])
	case "store":
		err = cmdStore(os.Args[2:])
	case "fetch":
		err = cmdFetch(os.Args[2:])
	case "query":
		err = cmdQuery(os.Args[2:])
	case "verify":
		err = cmdVerify(os.Args[2:])
	case "demo":
		err = cmdDemo(os.Args[2:])
	default:
		usage()
		os.Exit(2)
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "buck2cas:", err)
		os.Exit(1)
	}
}

func usage() {
	fmt.Fprintln(os.Stderr, `buck2cas — prototype Buck2-style CAS keyed by a Weft Context Root

usage:
  buck2cas init  --root DIR
  buck2cas store --root DIR --action-type T --toolchain C --input PATH=FILE ... --output FILE --output-path REL
  buck2cas fetch --root DIR --action-type T --toolchain C --input PATH=FILE ... --out DIR
  buck2cas query --root DIR [--action-type T] [--toolchain C] [--input PATH=FILE]
  buck2cas verify --root DIR
  buck2cas demo  --root DIR --receipt FILE`)
}

type inputFlags []string

func (i *inputFlags) String() string     { return strings.Join(*i, ",") }
func (i *inputFlags) Set(v string) error { *i = append(*i, v); return nil }

func actionFromFlags(actionType, toolchain string, flags inputFlags) (cas.Action, error) {
	action := cas.Action{ActionType: actionType, Toolchain: toolchain}
	for _, spec := range flags {
		at := strings.Index(spec, "=")
		if at <= 0 || at == len(spec)-1 {
			return cas.Action{}, fmt.Errorf("--input %q must be PATH=FILE", spec)
		}
		declared := spec[:at]
		file := spec[at+1:]
		digest, size, err := cas.HashFile(file)
		if err != nil {
			return cas.Action{}, err
		}
		action.Inputs = append(action.Inputs, cas.Input{Path: declared, Digest: digest, Bytes: size})
	}
	return cas.NormalizeAction(action)
}

func cmdInit(args []string) error {
	fs := flag.NewFlagSet("init", flag.ContinueOnError)
	root := fs.String("root", "", "CAS root")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" {
		return errors.New("init requires --root")
	}
	store, err := cas.Init(*root)
	if err != nil {
		return err
	}
	fmt.Printf("initialized %s (%s)\n", store.Root, strings.TrimSpace(cas.FormatLine))
	return nil
}

func cmdStore(args []string) error {
	fs := flag.NewFlagSet("store", flag.ContinueOnError)
	root := fs.String("root", "", "CAS root")
	actionType := fs.String("action-type", "", "typed action kind")
	toolchain := fs.String("toolchain", "", "toolchain identity")
	output := fs.String("output", "", "output file to store")
	outputPath := fs.String("output-path", "", "relative path inside the action output tree")
	asJSON := fs.Bool("json", false, "JSON output")
	var inputs inputFlags
	fs.Var(&inputs, "input", "PATH=FILE (repeatable)")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" || *actionType == "" || *toolchain == "" || *output == "" || *outputPath == "" {
		return errors.New("store requires --root/--action-type/--toolchain/--output/--output-path")
	}
	store, err := cas.Open(*root)
	if err != nil {
		return err
	}
	action, err := actionFromFlags(*actionType, *toolchain, inputs)
	if err != nil {
		return err
	}
	data, err := os.ReadFile(*output)
	if err != nil {
		return err
	}
	record, existed, err := store.StoreAction(action, data, *outputPath)
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(map[string]any{"existed": existed, "record": record})
	}
	fmt.Printf("stored key=%s digest=%s bytes=%d existed=%t\n",
		record.ContextRootID, record.OutputDigest, record.OutputBytes, existed)
	return nil
}

func cmdFetch(args []string) error {
	fs := flag.NewFlagSet("fetch", flag.ContinueOnError)
	root := fs.String("root", "", "CAS root")
	actionType := fs.String("action-type", "", "typed action kind")
	toolchain := fs.String("toolchain", "", "toolchain identity")
	out := fs.String("out", "", "materialization directory")
	asJSON := fs.Bool("json", false, "JSON output")
	var inputs inputFlags
	fs.Var(&inputs, "input", "PATH=FILE (repeatable)")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" || *actionType == "" || *toolchain == "" || *out == "" {
		return errors.New("fetch requires --root/--action-type/--toolchain/--out")
	}
	store, err := cas.Open(*root)
	if err != nil {
		return err
	}
	action, err := actionFromFlags(*actionType, *toolchain, inputs)
	if err != nil {
		return err
	}
	record, target, err := store.Fetch(action, *out)
	if errors.Is(err, cas.ErrMiss) {
		if *asJSON {
			_ = printJSON(map[string]any{"hit": false, "reason": "action cache miss"})
		}
		return fmt.Errorf("miss: no action record for the derived Context Root")
	}
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(map[string]any{"hit": true, "record": record, "materialized": target})
	}
	fmt.Printf("hit key=%s digest=%s materialized=%s\n",
		record.ContextRootID, record.OutputDigest, target)
	return nil
}

func cmdQuery(args []string) error {
	fs := flag.NewFlagSet("query", flag.ContinueOnError)
	root := fs.String("root", "", "CAS root")
	actionType := fs.String("action-type", "", "typed action kind filter")
	toolchain := fs.String("toolchain", "", "toolchain filter")
	asJSON := fs.Bool("json", false, "JSON output")
	var inputs inputFlags
	fs.Var(&inputs, "input", "PATH=FILE filter")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" {
		return errors.New("query requires --root")
	}
	store, err := cas.Open(*root)
	if err != nil {
		return err
	}
	inputPath, inputDigest := "", ""
	if len(inputs) > 0 {
		at := strings.Index(inputs[0], "=")
		if at <= 0 || at == len(inputs[0])-1 {
			return fmt.Errorf("--input %q must be PATH=FILE", inputs[0])
		}
		inputPath = inputs[0][:at]
		digest, _, err := cas.HashFile(inputs[0][at+1:])
		if err != nil {
			return err
		}
		inputDigest = digest
	}
	records, err := store.Query(*actionType, *toolchain, inputPath, inputDigest)
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(map[string]any{"count": len(records), "records": records})
	}
	fmt.Printf("count=%d\n", len(records))
	for _, record := range records {
		fmt.Printf("%s %s %s %s\n",
			record.ContextRootID, record.ActionType, record.Toolchain, record.OutputDigest)
	}
	return nil
}

func cmdVerify(args []string) error {
	fs := flag.NewFlagSet("verify", flag.ContinueOnError)
	root := fs.String("root", "", "CAS root")
	asJSON := fs.Bool("json", false, "JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" {
		return errors.New("verify requires --root")
	}
	store, err := cas.Open(*root)
	if err != nil {
		return err
	}
	count, err := store.Verify()
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(map[string]any{"ok": true, "records": count})
	}
	fmt.Printf("verify ok: %d record(s), all blobs and derived keys match\n", count)
	return nil
}

func printJSON(value any) error {
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	return encoder.Encode(value)
}

type assertion struct {
	Name   string `json:"name"`
	Pass   bool   `json:"pass"`
	Detail string `json:"detail,omitempty"`
}

func inputDigest(file string) (string, int64, error) {
	return cas.HashFile(file)
}

func cmdDemo(args []string) error {
	fs := flag.NewFlagSet("demo", flag.ContinueOnError)
	root := fs.String("root", "", "CAS root")
	receiptPath := fs.String("receipt", "", "receipt JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" || *receiptPath == "" {
		return errors.New("demo requires --root and --receipt")
	}
	_ = os.RemoveAll(*root)
	store, err := cas.Init(*root)
	if err != nil {
		return err
	}
	work := filepath.Join(*root, "work")
	if err := os.MkdirAll(work, 0o755); err != nil {
		return err
	}
	inputA := filepath.Join(work, "input-a.txt")
	if err := os.WriteFile(inputA, []byte("typed action input A\n"), 0o644); err != nil {
		return err
	}
	inputB := filepath.Join(work, "input-b.txt")
	if err := os.WriteFile(inputB, []byte("typed action input B\n"), 0o644); err != nil {
		return err
	}
	output := filepath.Join(work, "output.bin")
	outputBytes := []byte("deterministic object bytes for the action context\n")
	if err := os.WriteFile(output, outputBytes, 0o644); err != nil {
		return err
	}
	action := func(toolchain, input string) (cas.Action, error) {
		digest, size, err := inputDigest(input)
		if err != nil {
			return cas.Action{}, err
		}
		return cas.Action{
			ActionType: "compile_object",
			Toolchain:  toolchain,
			Inputs:     []cas.Input{{Path: "src/input.txt", Digest: digest, Bytes: size}},
		}, nil
	}
	a1, err := action("rust-1.85.0", inputA)
	if err != nil {
		return err
	}
	a2, err := action("rust-1.85.0", inputA)
	if err != nil {
		return err
	}
	a3, err := action("rust-1.86.0", inputA)
	if err != nil {
		return err
	}
	a4, err := action("rust-1.85.0", inputB)
	if err != nil {
		return err
	}

	key1, err := cas.DeriveKey(a1)
	if err != nil {
		return err
	}
	key2, err := cas.DeriveKey(a2)
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
	var assertions []assertion
	add := func(name string, pass bool, detail string) {
		assertions = append(assertions, assertion{Name: name, Pass: pass, Detail: detail})
	}
	add("same action context -> same Context Root",
		key1.ContextRootID == key2.ContextRootID,
		key1.ContextRootID+" == "+key2.ContextRootID)
	add("different toolchain -> different Context Root",
		key1.ContextRootID != key3.ContextRootID,
		key1.ContextRootID+" != "+key3.ContextRootID)
	add("different input -> different Context Root",
		key1.ContextRootID != key4.ContextRootID,
		key1.ContextRootID+" != "+key4.ContextRootID)

	record1, existed1, err := store.StoreAction(a1, outputBytes, "out/object.bin")
	if err != nil {
		return err
	}
	record2, existed2, err := store.StoreAction(a2, outputBytes, "out/object.bin")
	if err != nil {
		return err
	}
	add("first store is new", !existed1, fmt.Sprintf("existed=%t", existed1))
	add("second store is a hit", existed2, fmt.Sprintf("existed=%t", existed2))
	add("same key returns identical record digest",
		record1.OutputDigest == record2.OutputDigest,
		record1.OutputDigest+" == "+record2.OutputDigest)

	fetchDir := filepath.Join(work, "fetch")
	fetched, target, err := store.Fetch(a2, fetchDir)
	if err != nil {
		return err
	}
	fetchedBytes, err := os.ReadFile(target)
	if err != nil {
		return err
	}
	add("fetch materializes exact bytes",
		string(fetchedBytes) == string(outputBytes),
		fmt.Sprintf("%d bytes, digest %s", len(fetchedBytes), fetched.OutputDigest))
	add("fetch digest matches stored output", fetched.OutputDigest == record1.OutputDigest,
		fetched.OutputDigest)

	_, _, err = store.Fetch(a3, filepath.Join(work, "miss-toolchain"))
	add("different toolchain misses", errors.Is(err, cas.ErrMiss), fmt.Sprint(err))
	_, _, err = store.Fetch(a4, filepath.Join(work, "miss-input"))
	add("different input misses", errors.Is(err, cas.ErrMiss), fmt.Sprint(err))

	byType, err := store.Query("compile_object", "", "", "")
	if err != nil {
		return err
	}
	add("query by action type finds one record", len(byType) == 1, fmt.Sprintf("count=%d", len(byType)))
	byToolchain, err := store.Query("", "rust-1.85.0", "", "")
	if err != nil {
		return err
	}
	add("query by toolchain finds one record", len(byToolchain) == 1, fmt.Sprintf("count=%d", len(byToolchain)))
	noToolchain, err := store.Query("", "rust-1.86.0", "", "")
	if err != nil {
		return err
	}
	add("query by absent toolchain finds none", len(noToolchain) == 0, fmt.Sprintf("count=%d", len(noToolchain)))
	inputDigestValue, _, err := inputDigest(inputA)
	if err != nil {
		return err
	}
	byInput, err := store.Query("", "", "src/input.txt", inputDigestValue)
	if err != nil {
		return err
	}
	add("query by input digest finds one record", len(byInput) == 1, fmt.Sprintf("count=%d", len(byInput)))

	verified, err := store.Verify()
	if err != nil {
		return err
	}
	add("verify re-derives every key and blob", verified == 1, fmt.Sprintf("records=%d", verified))

	pass := true
	for _, item := range assertions {
		if !item.Pass {
			pass = false
		}
	}
	receipt := map[string]any{
		"kind":                     "buck2-cas-prototype",
		"schema":                   "weft.buck2-cas.receipt/1",
		"timestamp_utc":            time.Now().UTC().Format(time.RFC3339),
		"prototype":                "local CAS service + Buck2-compatible key/digest mapping; not a Buck2 plugin, not REAPI gRPC",
		"root":                     *root,
		"action":                   a1,
		"key":                      key1,
		"key_with_other_toolchain": key3,
		"key_with_other_input":     key4,
		"record":                   record1,
		"fetch": map[string]any{
			"materialized": target,
			"bytes":        len(fetchedBytes),
			"digest":       fetched.OutputDigest,
			"same_bytes":   string(fetchedBytes) == string(outputBytes),
		},
		"query": map[string]any{
			"by_action_type":      len(byType),
			"by_toolchain":        len(byToolchain),
			"by_other_input":      len(byInput),
			"by_absent_toolchain": len(noToolchain),
		},
		"assertions": assertions,
		"result":     map[string]any{"pass": pass, "total": len(assertions)},
	}
	encoded, err := json.MarshalIndent(receipt, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(*receiptPath), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(*receiptPath, append(encoded, '\n'), 0o644); err != nil {
		return err
	}
	fmt.Printf("buck2-cas demo: %d/%d assertions pass; receipt %s\n",
		countPass(assertions), len(assertions), *receiptPath)
	if !pass {
		return errors.New("demo assertions failed")
	}
	return nil
}

func countPass(items []assertion) int {
	n := 0
	for _, item := range items {
		if item.Pass {
			n++
		}
	}
	return n
}
