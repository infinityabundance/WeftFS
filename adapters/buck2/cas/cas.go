// Package cas is a local, file-backed content-addressed store whose action key
// is a typed Weft Context Root. It is a prototype adapter for a Buck2-style
// action cache, not a Buck2 plugin and not a REAPI gRPC server.
//
// Two addresses are kept deliberately:
//
//   - action key: a real weft.context-root.v1 record whose pinned "snapshot" is
//     the digest of the typed action context (action type, toolchain, inputs);
//   - blob digest: SHA-256, addressed as `sha256:<hex>` and stored at
//     cas/sha256/<first2>/<hex>, matching the REAPI bytestream blob layout.
//
// Buck2/REAPI supports BLAKE3 as a digest function; this prototype uses
// SHA-256 for the blob address because Buck2's default CAS digest is SHA-256.
// The action key itself remains a Weft Context Root and is not a Buck2 key.
package cas

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path"
	"path/filepath"
	"sort"
	"strings"

	"github.com/infinityabundance/WeftCodec-Go/context"
	"github.com/infinityabundance/WeftCodec-Go/hash"
	"github.com/infinityabundance/WeftCodec-Go/limits"
	"github.com/infinityabundance/WeftCodec-Go/selector"
	"github.com/infinityabundance/WeftCodec-Go/wcb"
)

const (
	FormatLine    = "weft-buck2-cas 1\n"
	ActionSchema  = "weft.buck2.action-context.v1"
	KeyDomain     = "weft/buck2/action/v1"
	Namespace     = "buck2"
	CASFormatName = "sha256"
)

// Input is one declared action input.
type Input struct {
	Path   string `json:"path"`
	Digest string `json:"digest"`
	Bytes  int64  `json:"bytes"`
}

// Action is the typed action context that produces the key.
type Action struct {
	ActionType string  `json:"action_type"`
	Toolchain  string  `json:"toolchain"`
	Inputs     []Input `json:"inputs"`
}

// Key is the derived identity of an action context.
type Key struct {
	ContextRootID string `json:"context_root_id"`
	URI           string `json:"uri"`
	ActionDigest  string `json:"action_digest"`
	SelectorID    string `json:"selector_id"`
	ContextCanon  string `json:"context_canonical_hex"`
}

// Record is the stored action-cache entry.
type Record struct {
	Schema        string  `json:"schema"`
	ActionType    string  `json:"action_type"`
	Toolchain     string  `json:"toolchain"`
	Inputs        []Input `json:"inputs"`
	OutputPath    string  `json:"output_path"`
	OutputDigest  string  `json:"output_digest"`
	OutputBytes   int64   `json:"output_bytes"`
	ContextRootID string  `json:"context_root_id"`
	URI           string  `json:"uri"`
	ActionDigest  string  `json:"action_digest"`
	SelectorID    string  `json:"selector_id"`
	ContextCanon  string  `json:"context_canonical_hex"`
}

// Store is a local CAS root.
type Store struct {
	Root string
	Lim  limits.Limits
}

// Open validates the format marker.
func Open(root string) (*Store, error) {
	s := &Store{Root: root, Lim: limits.Default()}
	data, err := os.ReadFile(filepath.Join(root, "FORMAT"))
	if err != nil {
		return nil, fmt.Errorf("open %s: %w (run init)", root, err)
	}
	if string(data) != FormatLine {
		return nil, fmt.Errorf("open %s: unsupported format %q", root, data)
	}
	return s, nil
}

// Init creates an empty store.
func Init(root string) (*Store, error) {
	if err := os.MkdirAll(filepath.Join(root, "cas", CASFormatName), 0o755); err != nil {
		return nil, err
	}
	if err := os.MkdirAll(filepath.Join(root, "actions"), 0o755); err != nil {
		return nil, err
	}
	if err := os.WriteFile(filepath.Join(root, "FORMAT"), []byte(FormatLine), 0o644); err != nil {
		return nil, err
	}
	return Open(root)
}

// BlobDigest returns the `sha256:<hex>` address of data.
func BlobDigest(data []byte) string {
	sum := sha256.Sum256(data)
	return CASFormatName + ":" + hex.EncodeToString(sum[:])
}

func digestHex(digest string) (string, error) {
	prefix := CASFormatName + ":"
	if !strings.HasPrefix(digest, prefix) {
		return "", fmt.Errorf("digest %q is not %s:<hex>", digest, CASFormatName)
	}
	hexPart := digest[len(prefix):]
	if len(hexPart) != 64 {
		return "", fmt.Errorf("digest %q has %d hex chars, want 64", digest, len(hexPart))
	}
	if _, err := hex.DecodeString(hexPart); err != nil {
		return "", fmt.Errorf("digest %q: %w", digest, err)
	}
	return hexPart, nil
}

// NormalizeAction validates the action context and sorts inputs by path.
func NormalizeAction(a Action) (Action, error) {
	if strings.TrimSpace(a.ActionType) == "" {
		return Action{}, errors.New("action type must not be empty")
	}
	if strings.TrimSpace(a.Toolchain) == "" {
		return Action{}, errors.New("toolchain must not be empty")
	}
	seen := map[string]bool{}
	inputs := make([]Input, 0, len(a.Inputs))
	for _, in := range a.Inputs {
		clean := path.Clean(strings.ReplaceAll(in.Path, "\\", "/"))
		if clean == "." || clean == "" || strings.HasPrefix(clean, "/") ||
			clean == ".." || strings.HasPrefix(clean, "../") || strings.Contains(clean, "/../") {
			return Action{}, fmt.Errorf("input path %q must be a clean relative path", in.Path)
		}
		if seen[clean] {
			return Action{}, fmt.Errorf("duplicate input path %q", clean)
		}
		seen[clean] = true
		if _, err := digestHex(in.Digest); err != nil {
			return Action{}, fmt.Errorf("input %s: %w", clean, err)
		}
		inputs = append(inputs, Input{Path: clean, Digest: in.Digest, Bytes: in.Bytes})
	}
	sort.Slice(inputs, func(i, j int) bool { return inputs[i].Path < inputs[j].Path })
	return Action{ActionType: a.ActionType, Toolchain: a.Toolchain, Inputs: inputs}, nil
}

// ActionBytes is the canonical WCB preimage of the action context digest.
func ActionBytes(a Action) ([]byte, error) {
	items := make([]wcb.Value, 0, len(a.Inputs))
	for _, in := range a.Inputs {
		items = append(items, wcb.MapValue(map[string]wcb.Value{
			"path":   wcb.TextValue(in.Path),
			"digest": wcb.TextValue(in.Digest),
			"bytes":  wcb.U64Value(uint64(in.Bytes)),
		}))
	}
	value := wcb.MapValue(map[string]wcb.Value{
		"schema":      wcb.TextValue(ActionSchema),
		"action_type": wcb.TextValue(a.ActionType),
		"toolchain":   wcb.TextValue(a.Toolchain),
		"inputs":      wcb.ArrayValue(items),
	})
	return wcb.Encode(value, limits.Default())
}

// DeriveKey computes the real Context Root for an action context.
func DeriveKey(a Action) (Key, error) {
	normalized, err := NormalizeAction(a)
	if err != nil {
		return Key{}, err
	}
	preimage, err := ActionBytes(normalized)
	if err != nil {
		return Key{}, err
	}
	actionDigest := hash.Domain(KeyDomain, preimage)
	sel, err := selector.ParseWith("select true", Namespace, limits.Default())
	if err != nil {
		return Key{}, err
	}
	root, err := context.NewContextRoot(
		sel, actionDigest, context.DefaultResolver, nil, nil, nil)
	if err != nil {
		return Key{}, err
	}
	id, err := root.ID()
	if err != nil {
		return Key{}, err
	}
	uri, err := root.AuthoritativeURI()
	if err != nil {
		return Key{}, err
	}
	canon, err := root.CanonicalHex(limits.Default())
	if err != nil {
		return Key{}, err
	}
	selectorID, err := sel.SelectorID(limits.Default())
	if err != nil {
		return Key{}, err
	}
	return Key{
		ContextRootID: id.Hex(),
		URI:           uri,
		ActionDigest:  actionDigest.Hex(),
		SelectorID:    selectorID.Hex(),
		ContextCanon:  canon,
	}, nil
}

func (s *Store) actionPath(id string) string {
	return filepath.Join(s.Root, "actions", id+".json")
}

func (s *Store) blobPath(digest string) (string, error) {
	hexPart, err := digestHex(digest)
	if err != nil {
		return "", err
	}
	return filepath.Join(s.Root, "cas", CASFormatName, hexPart[:2], hexPart), nil
}

func (s *Store) writeBlob(data []byte) (string, error) {
	digest := BlobDigest(data)
	target, err := s.blobPath(digest)
	if err != nil {
		return "", err
	}
	if existing, err := os.ReadFile(target); err == nil {
		if BlobDigest(existing) != digest {
			return "", fmt.Errorf("CAS blob %s is corrupt", target)
		}
		return digest, nil
	}
	if err := os.MkdirAll(filepath.Dir(target), 0o755); err != nil {
		return "", err
	}
	tmp, err := os.CreateTemp(filepath.Dir(target), ".blob-*")
	if err != nil {
		return "", err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return "", err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return "", err
	}
	if err := tmp.Close(); err != nil {
		return "", err
	}
	if err := os.Rename(tmpName, target); err != nil {
		return "", err
	}
	return digest, nil
}

// StoreAction stores an output blob and its action-cache record. Storing the
// same action key with different output bytes fails closed.
func (s *Store) StoreAction(a Action, output []byte, outputPath string) (Record, bool, error) {
	normalized, err := NormalizeAction(a)
	if err != nil {
		return Record{}, false, err
	}
	key, err := DeriveKey(normalized)
	if err != nil {
		return Record{}, false, err
	}
	cleanOut := path.Clean(strings.ReplaceAll(outputPath, "\\", "/"))
	if cleanOut == "." || cleanOut == "" || strings.HasPrefix(cleanOut, "/") ||
		cleanOut == ".." || strings.HasPrefix(cleanOut, "../") {
		return Record{}, false, fmt.Errorf("output path %q must be a clean relative path", outputPath)
	}
	digest, err := s.writeBlob(output)
	if err != nil {
		return Record{}, false, err
	}
	record := Record{
		Schema:        "weft.buck2.action-cache.v1",
		ActionType:    normalized.ActionType,
		Toolchain:     normalized.Toolchain,
		Inputs:        normalized.Inputs,
		OutputPath:    cleanOut,
		OutputDigest:  digest,
		OutputBytes:   int64(len(output)),
		ContextRootID: key.ContextRootID,
		URI:           key.URI,
		ActionDigest:  key.ActionDigest,
		SelectorID:    key.SelectorID,
		ContextCanon:  key.ContextCanon,
	}
	pathToRecord := s.actionPath(key.ContextRootID)
	if existingBytes, err := os.ReadFile(pathToRecord); err == nil {
		var existing Record
		if err := json.Unmarshal(existingBytes, &existing); err != nil {
			return Record{}, false, fmt.Errorf("existing record is corrupt: %w", err)
		}
		if existing.OutputDigest != record.OutputDigest {
			return Record{}, false, fmt.Errorf(
				"same action key %s has different output bytes (%s vs %s)",
				key.ContextRootID, existing.OutputDigest, record.OutputDigest)
		}
		return existing, true, nil
	}
	encoded, err := json.MarshalIndent(record, "", "  ")
	if err != nil {
		return Record{}, false, err
	}
	encoded = append(encoded, '\n')
	tmp, err := os.CreateTemp(filepath.Dir(pathToRecord), ".action-*")
	if err != nil {
		return Record{}, false, err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	if _, err := tmp.Write(encoded); err != nil {
		tmp.Close()
		return Record{}, false, err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return Record{}, false, err
	}
	if err := tmp.Close(); err != nil {
		return Record{}, false, err
	}
	if err := os.Rename(tmpName, pathToRecord); err != nil {
		return Record{}, false, err
	}
	return record, false, nil
}

// ErrMiss is returned when no action record exists for the derived key.
var ErrMiss = errors.New("action cache miss")

// Get returns the stored record for an action context.
func (s *Store) Get(a Action) (Record, error) {
	key, err := DeriveKey(a)
	if err != nil {
		return Record{}, err
	}
	data, err := os.ReadFile(s.actionPath(key.ContextRootID))
	if errors.Is(err, os.ErrNotExist) {
		return Record{}, ErrMiss
	}
	if err != nil {
		return Record{}, err
	}
	var record Record
	if err := json.Unmarshal(data, &record); err != nil {
		return Record{}, err
	}
	if record.ContextRootID != key.ContextRootID {
		return Record{}, fmt.Errorf("record key %s does not match derived key %s",
			record.ContextRootID, key.ContextRootID)
	}
	return record, nil
}

// ReadBlob reads and verifies one CAS blob.
func (s *Store) ReadBlob(digest string) ([]byte, error) {
	target, err := s.blobPath(digest)
	if err != nil {
		return nil, err
	}
	data, err := os.ReadFile(target)
	if err != nil {
		return nil, err
	}
	if got := BlobDigest(data); got != digest {
		return nil, fmt.Errorf("CAS blob %s has digest %s", digest, got)
	}
	return data, nil
}

// Fetch materializes an action output under outDir.
func (s *Store) Fetch(a Action, outDir string) (Record, string, error) {
	record, err := s.Get(a)
	if err != nil {
		return Record{}, "", err
	}
	data, err := s.ReadBlob(record.OutputDigest)
	if err != nil {
		return Record{}, "", err
	}
	target := filepath.Join(outDir, filepath.FromSlash(record.OutputPath))
	if err := os.MkdirAll(filepath.Dir(target), 0o755); err != nil {
		return Record{}, "", err
	}
	if err := os.WriteFile(target, data, 0o644); err != nil {
		return Record{}, "", err
	}
	if got := BlobDigest(data); got != record.OutputDigest {
		return Record{}, "", fmt.Errorf("materialized output digest %s != %s", got, record.OutputDigest)
	}
	return record, target, nil
}

// Query filters stored records by action context. Empty filters match all.
func (s *Store) Query(actionType, toolchain, inputPath, inputDigest string) ([]Record, error) {
	entries, err := os.ReadDir(filepath.Join(s.Root, "actions"))
	if err != nil {
		return nil, err
	}
	var records []Record
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}
		data, err := os.ReadFile(filepath.Join(s.Root, "actions", entry.Name()))
		if err != nil {
			return nil, err
		}
		var record Record
		if err := json.Unmarshal(data, &record); err != nil {
			return nil, fmt.Errorf("%s: %w", entry.Name(), err)
		}
		if actionType != "" && record.ActionType != actionType {
			continue
		}
		if toolchain != "" && record.Toolchain != toolchain {
			continue
		}
		if inputPath != "" || inputDigest != "" {
			matched := false
			for _, in := range record.Inputs {
				if inputPath != "" && in.Path != inputPath {
					continue
				}
				if inputDigest != "" && in.Digest != inputDigest {
					continue
				}
				matched = true
				break
			}
			if !matched {
				continue
			}
		}
		records = append(records, record)
	}
	sort.Slice(records, func(i, j int) bool {
		return records[i].ContextRootID < records[j].ContextRootID
	})
	return records, nil
}

// Verify checks every record and blob, and re-derives every key.
func (s *Store) Verify() (int, error) {
	records, err := s.Query("", "", "", "")
	if err != nil {
		return 0, err
	}
	for _, record := range records {
		inputs := make([]Input, len(record.Inputs))
		copy(inputs, record.Inputs)
		key, err := DeriveKey(Action{
			ActionType: record.ActionType,
			Toolchain:  record.Toolchain,
			Inputs:     inputs,
		})
		if err != nil {
			return 0, err
		}
		if key.ContextRootID != record.ContextRootID {
			return 0, fmt.Errorf("record %s re-derives as %s", record.ContextRootID, key.ContextRootID)
		}
		data, err := s.ReadBlob(record.OutputDigest)
		if err != nil {
			return 0, err
		}
		if int64(len(data)) != record.OutputBytes {
			return 0, fmt.Errorf("record %s output is %d bytes, want %d",
				record.ContextRootID, len(data), record.OutputBytes)
		}
	}
	return len(records), nil
}

// HashFile returns the SHA-256 digest and size of a file.
func HashFile(path string) (string, int64, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", 0, err
	}
	defer f.Close()
	h := sha256.New()
	n, err := io.Copy(h, f)
	if err != nil {
		return "", 0, err
	}
	return CASFormatName + ":" + hex.EncodeToString(h.Sum(nil)), n, nil
}
