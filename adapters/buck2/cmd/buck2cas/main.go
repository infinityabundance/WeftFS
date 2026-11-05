// Command buck2cas is a local action-cache adapter keyed by a typed Weft
// action declaration and stored on WeftFS. It is not a Buck2 plugin, does not
// load into Buck2, and does not speak REAPI gRPC.
package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path"
	"sort"
	"strings"

	"github.com/infinityabundance/WeftFS-Go/store"
	"github.com/infinityabundance/WeftFS-Go/werr"
	"github.com/infinityabundance/WeftFS/adapters/buck2/cas"
)

const (
	exitOK       = 0
	exitFailure  = 1
	exitUsage    = 2
	exitMiss     = 3
	exitMismatch = 4
	exitConflict = 5
)

func main() {
	os.Exit(run(os.Args[1:]))
}

func run(args []string) int {
	if len(args) < 1 {
		usage()
		return exitUsage
	}
	var err error
	switch args[0] {
	case "init":
		err = cmdInit(args[1:])
	case "key":
		err = cmdKey(args[1:])
	case "store":
		err = cmdStore(args[1:])
	case "fetch":
		err = cmdFetch(args[1:])
	case "query":
		err = cmdQuery(args[1:])
	case "verify":
		err = cmdVerify(args[1:])
	case "gc":
		err = cmdGC(args[1:])
	case "context":
		err = cmdContext(args[1:])
	case "resolve":
		err = cmdResolve(args[1:])
	case "delete":
		err = cmdDelete(args[1:])
	case "demo-store":
		err = cmdDemoStore(args[1:])
	case "bench":
		err = cmdBench(args[1:])
	case "help", "-h", "--help":
		usage()
		return exitOK
	default:
		usage()
		return exitUsage
	}
	if err == nil {
		return exitOK
	}
	code := exitFailure
	if errors.Is(err, cas.ErrMiss) {
		code = exitMiss
	}
	var typed *werr.Error
	if errors.As(err, &typed) {
		switch typed.Kind {
		case werr.Conflict:
			code = exitConflict
		case werr.Corrupt, werr.Malformed, werr.NonCanonical:
			if code == exitFailure {
				code = exitMismatch
			}
		}
	}
	fmt.Fprintln(os.Stderr, "buck2cas:", err)
	return code
}

func usage() {
	fmt.Fprint(os.Stderr, `buck2cas — WeftFS-backed Buck2-style action cache (not a Buck2 plugin)

usage:
  buck2cas init    --root DIR [--gc-grace-secs N] [--checkpoint-interval N]
  buck2cas key     --key-file FILE [--json]
  buck2cas store   --root DIR --key-file FILE --blob FILE [--output-path REL] [--json]
  buck2cas fetch   --root DIR --key HEX64 --out FILE [--json]
  buck2cas query   --root DIR --by action=HEX64 [--json]
  buck2cas verify  --root DIR (--key HEX64 [--blob FILE] | --all) [--json]
  buck2cas context --root DIR --key HEX64 [--name NAME] [--snapshot NAME] [--replace] [--json]
  buck2cas resolve --root DIR --name NAME [--json]
  buck2cas delete  --root DIR --key HEX64
  buck2cas gc      --root DIR [--apply] [--grace-secs N] [--max-deletes N] [--json]
  buck2cas demo-store --root DIR --receipt FILE [--rust-bin FILE]
  buck2cas bench   --root DIR --small N --large N --out FILE

exit codes: 0 success, 1 failure, 2 usage, 3 cache miss, 4 verification
mismatch/corrupt store data, 5 same-key/different-bytes conflict.
`)
}

type keyFileInput struct {
	Path      string `json:"path"`
	File      string `json:"file,omitempty"`
	ContentID string `json:"content_id,omitempty"`
	Bytes     *int64 `json:"bytes,omitempty"`
}

type keyFile struct {
	ActionType string            `json:"action_type"`
	Toolchain  string            `json:"toolchain"`
	Inputs     []keyFileInput    `json:"inputs"`
	Params     map[string]string `json:"params,omitempty"`
	OutputPath string            `json:"output_path,omitempty"`
}

func loadKeyFile(file string) (cas.Action, string, error) {
	data, err := os.ReadFile(file)
	if err != nil {
		return cas.Action{}, "", err
	}
	var spec keyFile
	if err := json.Unmarshal(data, &spec); err != nil {
		return cas.Action{}, "", fmt.Errorf("%s: %w", file, err)
	}
	action := cas.Action{
		ActionType: spec.ActionType,
		Toolchain:  spec.Toolchain,
		Params:     spec.Params,
	}
	for _, in := range spec.Inputs {
		input := cas.Input{Path: in.Path, ContentID: in.ContentID}
		if in.Bytes != nil {
			input.Bytes = *in.Bytes
		}
		if in.File != "" {
			contentID, bytes, err := hashFile(in.File)
			if err != nil {
				return cas.Action{}, "", err
			}
			if in.ContentID != "" && strings.ToLower(in.ContentID) != contentID {
				return cas.Action{}, "", fmt.Errorf(
					"input %s declares content id %s but %s hashes to %s",
					in.Path, in.ContentID, in.File, contentID)
			}
			if in.Bytes != nil && *in.Bytes != bytes {
				return cas.Action{}, "", fmt.Errorf(
					"input %s declares %d bytes but %s is %d bytes",
					in.Path, *in.Bytes, in.File, bytes)
			}
			input.ContentID = contentID
			input.Bytes = bytes
		}
		action.Inputs = append(action.Inputs, input)
	}
	normalized, err := cas.NormalizeAction(action)
	if err != nil {
		return cas.Action{}, "", err
	}
	return normalized, spec.OutputPath, nil
}

func hashFile(file string) (string, int64, error) {
	f, err := os.Open(file)
	if err != nil {
		return "", 0, err
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil {
		return "", 0, err
	}
	contentID, err := cas.ContentIDReader(f)
	if err != nil {
		return "", 0, err
	}
	return contentID, info.Size(), nil
}

func cmdInit(args []string) error {
	fs := flagSet("init")
	root := fs.String("root", "", "WeftFS store root")
	gcGrace := fs.Uint64("gc-grace-secs", 3600, "GC grace window in seconds")
	checkpoint := fs.Uint64("checkpoint-interval", 256, "commit checkpoint interval")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" {
		return errors.New("init requires --root")
	}
	opts := store.DefaultOptions()
	opts.GCGraceSecs = *gcGrace
	opts.CheckpointInterval = *checkpoint
	s, err := cas.Init(*root, opts)
	if err != nil {
		return err
	}
	fmt.Printf("initialized %s generation=%d head=%s\n", s.Root(), s.Generation(), s.Head())
	return s.Close()
}

func cmdKey(args []string) error {
	fs := flag.NewFlagSet("key", flag.ContinueOnError)
	keyFile := fs.String("key-file", "", "action declaration JSON")
	asJSON := fs.Bool("json", false, "JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *keyFile == "" {
		return errors.New("key requires --key-file")
	}
	action, _, err := loadKeyFile(*keyFile)
	if err != nil {
		return err
	}
	key, err := cas.DeriveKey(action)
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(key)
	}
	fmt.Printf("action_key=%s\nrecord_object_id=%s\noutput_object_id=%s\nselector=%s\n",
		key.ActionKey, key.RecordObjectID, key.OutputObjectID, key.ContextSelector)
	return nil
}

func cmdStore(args []string) error {
	fs := flag.NewFlagSet("store", flag.ContinueOnError)
	root := fs.String("root", "", "WeftFS store root")
	specFile := fs.String("key-file", "", "action declaration JSON")
	blobFile := fs.String("blob", "", "output blob to publish")
	outputPath := fs.String("output-path", "", "override the declared output path")
	asJSON := fs.Bool("json", false, "JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" || *specFile == "" || *blobFile == "" {
		return errors.New("store requires --root/--key-file/--blob")
	}
	action, declaredOut, err := loadKeyFile(*specFile)
	if err != nil {
		return err
	}
	if *outputPath != "" {
		declaredOut = *outputPath
	}
	if declaredOut == "" {
		return errors.New("store requires an output path (--output-path or key-file output_path)")
	}
	blob, err := os.ReadFile(*blobFile)
	if err != nil {
		return err
	}
	s, err := cas.Open(*root)
	if err != nil {
		return err
	}
	defer s.Close()
	record, existed, err := s.StoreAction(action, blob, declaredOut)
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(map[string]any{"existed": existed, "record": record})
	}
	fmt.Printf("stored key=%s content_id=%s bytes=%d existed=%t\n",
		record.ActionKey, record.OutputContentID, record.OutputBytes, existed)
	return nil
}

func cmdFetch(args []string) error {
	fs := flag.NewFlagSet("fetch", flag.ContinueOnError)
	root := fs.String("root", "", "WeftFS store root")
	key := fs.String("key", "", "action key (64 lowercase hex)")
	out := fs.String("out", "", "materialize the exact output to this file")
	asJSON := fs.Bool("json", false, "JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" || *key == "" || *out == "" {
		return errors.New("fetch requires --root/--key/--out")
	}
	s, err := cas.OpenReadOnly(*root)
	if err != nil {
		return err
	}
	defer s.Close()
	record, data, err := s.Fetch(*key, *out)
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(map[string]any{
			"hit":        true,
			"key":        record.ActionKey,
			"content_id": record.OutputContentID,
			"bytes":      len(data),
			"out":        *out,
		})
	}
	fmt.Printf("hit key=%s content_id=%s bytes=%d out=%s\n",
		record.ActionKey, record.OutputContentID, len(data), *out)
	return nil
}

func cmdQuery(args []string) error {
	fs := flag.NewFlagSet("query", flag.ContinueOnError)
	root := fs.String("root", "", "WeftFS store root")
	by := fs.String("by", "", "action=<key>")
	asJSON := fs.Bool("json", false, "JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" || *by == "" {
		return errors.New("query requires --root/--by action=<key>")
	}
	key, err := parseByAction(*by)
	if err != nil {
		return err
	}
	s, err := cas.OpenReadOnly(*root)
	if err != nil {
		return err
	}
	defer s.Close()
	result, err := s.QueryByAction(key)
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(result)
	}
	fmt.Printf("count=1\naction_key=%s\ncontent_id=%s\nobject_id=%s\npath=%s\nbytes=%d\nselector=%s\n",
		result.ActionKey, result.ContentID, result.OutputObjectID,
		result.StorePath, result.Bytes, result.Selector)
	return nil
}

func cmdVerify(args []string) error {
	fs := flag.NewFlagSet("verify", flag.ContinueOnError)
	root := fs.String("root", "", "WeftFS store root")
	key := fs.String("key", "", "action key (64 lowercase hex)")
	blobFile := fs.String("blob", "", "external blob to re-hash and compare")
	all := fs.Bool("all", false, "verify every action record")
	asJSON := fs.Bool("json", false, "JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" {
		return errors.New("verify requires --root")
	}
	s, err := cas.OpenReadOnly(*root)
	if err != nil {
		return err
	}
	defer s.Close()
	if *all {
		count, err := s.VerifyAll()
		if err != nil {
			return err
		}
		if *asJSON {
			return printJSON(map[string]any{"ok": true, "records": count})
		}
		fmt.Printf("verify ok: %d action record(s)\n", count)
		return nil
	}
	if *key == "" {
		return errors.New("verify requires --key or --all")
	}
	var blob []byte
	if *blobFile != "" {
		blob, err = os.ReadFile(*blobFile)
		if err != nil {
			return err
		}
	}
	record, err := s.Verify(*key, blob)
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(map[string]any{
			"ok":         true,
			"key":        record.ActionKey,
			"content_id": record.OutputContentID,
			"bytes":      record.OutputBytes,
		})
	}
	fmt.Printf("verify ok: key=%s content_id=%s bytes=%d\n",
		record.ActionKey, record.OutputContentID, record.OutputBytes)
	return nil
}

func cmdContext(args []string) error {
	fs := flag.NewFlagSet("context", flag.ContinueOnError)
	root := fs.String("root", "", "WeftFS store root")
	key := fs.String("key", "", "action key (64 lowercase hex)")
	name := fs.String("name", "", "Context Root name (default by:action=<key>)")
	snapshot := fs.String("snapshot", "", "snapshot name or commit hex (default current head)")
	replace := fs.Bool("replace", false, "replace an existing same-name Context Root")
	asJSON := fs.Bool("json", false, "JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" || *key == "" {
		return errors.New("context requires --root/--key")
	}
	s, err := cas.Open(*root)
	if err != nil {
		return err
	}
	defer s.Close()
	info, err := s.CreateContext(*key, *name, *snapshot, *replace)
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(info)
	}
	fmt.Printf("name=%s\ncontext_root_id=%s\nuri=%s\nsnapshot_id=%s\nselector=%s\n",
		info.Name, info.ContextRootID, info.URI, info.SnapshotID, info.Selector)
	return nil
}

func cmdResolve(args []string) error {
	fs := flag.NewFlagSet("resolve", flag.ContinueOnError)
	root := fs.String("root", "", "WeftFS store root")
	name := fs.String("name", "", "Context Root name or URI")
	asJSON := fs.Bool("json", false, "JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" || *name == "" {
		return errors.New("resolve requires --root/--name")
	}
	s, err := cas.OpenReadOnly(*root)
	if err != nil {
		return err
	}
	defer s.Close()
	value, err := s.ResolveContext(*name, true)
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(value)
	}
	fmt.Printf("name=%s\ncontext_root_id=%s\nsnapshot_id=%s\nmatched=%d\nobject_ids=%s\ncontent_ids=%s\n",
		value.Name, value.ContextRootID, value.SnapshotID, value.MatchedCount,
		strings.Join(value.ObjectIDs, ","), strings.Join(value.ContentIDs, ","))
	return nil
}

func cmdDelete(args []string) error {
	fs := flag.NewFlagSet("delete", flag.ContinueOnError)
	root := fs.String("root", "", "WeftFS store root")
	key := fs.String("key", "", "action key (64 lowercase hex)")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" || *key == "" {
		return errors.New("delete requires --root/--key")
	}
	s, err := cas.Open(*root)
	if err != nil {
		return err
	}
	defer s.Close()
	if err := s.DeleteAction(*key); err != nil {
		return err
	}
	fmt.Printf("deleted action=%s\n", *key)
	return nil
}

func cmdGC(args []string) error {
	fs := flag.NewFlagSet("gc", flag.ContinueOnError)
	root := fs.String("root", "", "WeftFS store root")
	apply := fs.Bool("apply", false, "delete unreachable blobs (default dry run)")
	grace := fs.Int64("grace-secs", -1, "override the GC grace window")
	maxDeletes := fs.Int("max-deletes", 0, "delete at most N blobs (0 = unbounded)")
	asJSON := fs.Bool("json", false, "JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *root == "" {
		return errors.New("gc requires --root")
	}
	s, err := cas.Open(*root)
	if err != nil {
		return err
	}
	defer s.Close()
	if *grace >= 0 {
		if err := s.SetGCGrace(uint64(*grace)); err != nil {
			return err
		}
	}
	report, err := s.GC(*apply, *maxDeletes)
	if err != nil {
		return err
	}
	if *asJSON {
		return printJSON(report)
	}
	fmt.Printf("applied=%t reachable=%d files/%d bytes candidates=%d files/%d bytes deleted=%d files/%d bytes\n",
		report.Applied, report.ReachableFiles, report.ReachableBytes,
		report.CandidateFiles, report.CandidateBytes,
		report.DeletedFiles, report.DeletedBytes)
	return nil
}

func parseByAction(value string) (string, error) {
	if !strings.HasPrefix(value, "action=") {
		return "", fmt.Errorf("--by must be action=<key> (got %q)", value)
	}
	key := strings.TrimPrefix(value, "action=")
	if _, err := cas.ParseKey(key); err != nil {
		return "", err
	}
	return key, nil
}

func printJSON(value any) error {
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	return encoder.Encode(value)
}

func flagSet(name string) *flag.FlagSet {
	return flag.NewFlagSet(name, flag.ContinueOnError)
}

// sortedKeys is used by receipts and tests.
func sortedKeys(m map[string]string) []string {
	keys := make([]string, 0, len(m))
	for key := range m {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}

// copyAll is a small io helper for tests.
func copyAll(dst io.Writer, src io.Reader) error {
	_, err := io.Copy(dst, src)
	return err
}

// ensurePath is used by the demo to create host files deterministically.
func ensurePath(target string) error {
	return os.MkdirAll(path.Dir(target), 0o755)
}
