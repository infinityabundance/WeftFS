// Package cas implements a Buck2-style action cache on top of the WeftFS
// store. It is a local adapter, not a Buck2 plugin and not a REAPI gRPC
// server.
//
// Two identities are kept deliberately separate:
//
//   - ActionKey = H("weft/buck2/action/v1", WCB(action declaration)). The
//     declaration is the action type, toolchain, the sorted input content
//     roots, and canonical params. Paths are metadata, never key material.
//   - Blob address = the WeftFS whole-file ContentId
//     H("weft/chunk/v1", exact bytes). Every CAS blob is an ordinary WeftFS
//     file object, so one content-addressed primitive serves build outputs and
//     AI artifacts.
//
// The action record is itself a content-addressed WeftFS file. A typed
// relation `cas_action` points from the record object to the output object;
// the reverse `cas_output_of` edge makes `by:action=<key>` expressible as a
// first-class WeftQL selector.
package cas

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"io"
	"path"
	"sort"
	"strings"
	"unicode/utf8"

	"github.com/infinityabundance/WeftCodec-Go/hash"
	"github.com/infinityabundance/WeftCodec-Go/limits"
	"github.com/infinityabundance/WeftCodec-Go/wcb"
)

const (
	// ActionSchema is the canonical action-declaration schema string.
	ActionSchema = "weft.buck2.action.v1"
	// RecordSchema is the canonical action-record schema string.
	RecordSchema = "weft.buck2.action-record.v1"
	// KeyDomain is the action-key domain separation string.
	KeyDomain = "weft/buck2/action/v1"
	// Namespace is the selector namespace used by generated Context Roots. v1
	// stores have exactly one namespace, `default`.
	Namespace = "default"

	// PredicateAction points from the action-record object to its output.
	PredicateAction = "cas_action"
	// PredicateOutputOf points from the output object back to the action
	// record. It exists so the output can be selected by a one-hop relation
	// selector (WeftQL `relation` returns the edge subject).
	PredicateOutputOf = "cas_output_of"

	// AttrActionKey is set on the output object to the lowercase action key.
	AttrActionKey = "cas.action_key"
	// AttrActionRecord is set on the output object to the record ObjectId.
	AttrActionRecord = "cas.action_record"
	// AttrContentID is set on the output object to its ContentId.
	AttrContentID = "cas.content_id"
	// AttrOutputPath is set on the output object to its declared output path.
	AttrOutputPath = "cas.output_path"

	// RecordSeedPrefix and OutputSeedPrefix make ObjectIds deterministic from
	// the action key, so either language can reproduce the layout for tests.
	RecordSeedPrefix = "weft.buck2.record:"
	OutputSeedPrefix = "weft.buck2.output:"
	DirSeedPrefix    = "weft.buck2.dir:"

	// CASDir is the store directory that holds all adapter state.
	CASDir     = "/cas"
	RecordsDir = "/cas/records"
	OutputsDir = "/cas/outputs"

	// Bounds. They are intentionally small: a CAS action declaration is
	// metadata, never payload.
	MaxInputs       = 4096
	MaxParams       = 256
	MaxParamBytes   = 16 * 1024
	MaxPathBytes    = 4096
	MaxPathSegment  = 255
	MaxOutputBytes  = 1 << 30 // 1 GiB per CAS output in this adapter
	MaxRecordBytes  = 1 << 20
	MaxActionType   = 256
	MaxToolchain    = 256
	MaxContentIDLen = 64
)

// Input is one declared action input. ContentID is a WeftFS whole-file
// ContentId (BLAKE3), not a SHA-256 digest.
type Input struct {
	Path      string `json:"path"`
	ContentID string `json:"content_id"`
	Bytes     int64  `json:"bytes"`
}

// Action is the typed action declaration used to derive an ActionKey.
type Action struct {
	ActionType string            `json:"action_type"`
	Toolchain  string            `json:"toolchain"`
	Inputs     []Input           `json:"inputs"`
	Params     map[string]string `json:"params,omitempty"`
}

// Key is the derived action identity.
type Key struct {
	ActionKey       string `json:"action_key"`
	ActionPreimage  string `json:"action_preimage_hex"`
	RecordObjectID  string `json:"record_object_id"`
	OutputObjectID  string `json:"output_object_id"`
	ContextSelector string `json:"context_selector"`
}

// ContentID returns the WeftFS whole-file ContentId of data.
func ContentID(data []byte) string {
	return hash.Domain(hash.DomainChunk, data).Hex()
}

// ContentIDReader returns the WeftFS whole-file ContentId of a stream.
func ContentIDReader(r io.Reader) (string, error) {
	h, err := hash.DomainReader(hash.DomainChunk, r)
	if err != nil {
		return "", err
	}
	return h.Hex(), nil
}

func validateContentID(text string) (string, error) {
	if len(text) != MaxContentIDLen {
		return "", fmt.Errorf("content id %q must be %d lowercase hex characters", text, MaxContentIDLen)
	}
	normalized := strings.ToLower(text)
	if _, err := hex.DecodeString(normalized); err != nil {
		return "", fmt.Errorf("content id %q: %w", text, err)
	}
	return normalized, nil
}

func validateText(value, what string, max int) (string, error) {
	if strings.TrimSpace(value) == "" {
		return "", fmt.Errorf("%s must not be empty", what)
	}
	if !utf8.ValidString(value) {
		return "", fmt.Errorf("%s must be valid UTF-8", what)
	}
	if len(value) > max {
		return "", fmt.Errorf("%s is %d bytes, above the %d-byte bound", what, len(value), max)
	}
	return value, nil
}

// ValidateOutputPath requires a clean, relative, slash-separated path.
func ValidateOutputPath(value string) (string, error) {
	if value == "" {
		return "", fmt.Errorf("output path must not be empty")
	}
	if strings.ContainsRune(value, 0) {
		return "", fmt.Errorf("output path must not contain NUL")
	}
	clean := path.Clean(strings.ReplaceAll(value, "\\", "/"))
	if clean == "." || clean == "" || strings.HasPrefix(clean, "/") ||
		clean == ".." || strings.HasPrefix(clean, "../") || strings.Contains(clean, "/../") {
		return "", fmt.Errorf("output path %q must be a clean relative path", value)
	}
	if len(clean) > MaxPathBytes {
		return "", fmt.Errorf("output path is %d bytes, above the %d-byte bound", len(clean), MaxPathBytes)
	}
	for _, segment := range strings.Split(clean, "/") {
		if len(segment) > MaxPathSegment {
			return "", fmt.Errorf("output path segment %q exceeds %d bytes", segment, MaxPathSegment)
		}
	}
	return clean, nil
}

func validateInputPath(value string) (string, error) {
	clean := path.Clean(strings.ReplaceAll(value, "\\", "/"))
	if clean == "." || clean == "" || strings.HasPrefix(clean, "/") ||
		clean == ".." || strings.HasPrefix(clean, "../") || strings.Contains(clean, "/../") {
		return "", fmt.Errorf("input path %q must be a clean relative path", value)
	}
	if len(clean) > MaxPathBytes {
		return "", fmt.Errorf("input path is %d bytes, above the %d-byte bound", len(clean), MaxPathBytes)
	}
	return clean, nil
}

// NormalizeAction validates an action declaration and sorts its inputs by
// (content root, path). Sorting by content root is what makes the key
// independent of declaration order.
func NormalizeAction(a Action) (Action, error) {
	actionType, err := validateText(a.ActionType, "action type", MaxActionType)
	if err != nil {
		return Action{}, err
	}
	toolchain, err := validateText(a.Toolchain, "toolchain", MaxToolchain)
	if err != nil {
		return Action{}, err
	}
	if len(a.Inputs) > MaxInputs {
		return Action{}, fmt.Errorf("action declares %d inputs, above the %d-input bound", len(a.Inputs), MaxInputs)
	}
	params := make(map[string]string, len(a.Params))
	if len(a.Params) > MaxParams {
		return Action{}, fmt.Errorf("action declares %d params, above the %d-param bound", len(a.Params), MaxParams)
	}
	for key, value := range a.Params {
		if key == "" {
			return Action{}, fmt.Errorf("param name must not be empty")
		}
		if len(key)+len(value) > MaxParamBytes {
			return Action{}, fmt.Errorf("param %q exceeds the %d-byte bound", key, MaxParamBytes)
		}
		params[key] = value
	}
	seenPath := make(map[string]bool, len(a.Inputs))
	inputs := make([]Input, 0, len(a.Inputs))
	for _, in := range a.Inputs {
		clean, err := validateInputPath(in.Path)
		if err != nil {
			return Action{}, err
		}
		if seenPath[clean] {
			return Action{}, fmt.Errorf("duplicate input path %q", clean)
		}
		seenPath[clean] = true
		contentID, err := validateContentID(in.ContentID)
		if err != nil {
			return Action{}, fmt.Errorf("input %s: %w", clean, err)
		}
		if in.Bytes < 0 {
			return Action{}, fmt.Errorf("input %s has negative size", clean)
		}
		inputs = append(inputs, Input{Path: clean, ContentID: contentID, Bytes: in.Bytes})
	}
	sort.Slice(inputs, func(i, j int) bool {
		if inputs[i].ContentID != inputs[j].ContentID {
			return inputs[i].ContentID < inputs[j].ContentID
		}
		return inputs[i].Path < inputs[j].Path
	})
	return Action{ActionType: actionType, Toolchain: toolchain, Inputs: inputs, Params: params}, nil
}

// ActionPreimage returns the canonical WCB bytes hashed to derive the key.
// The preimage is exactly {schema, action_type, toolchain, inputs (sorted
// content roots), params}. Declared input paths are deliberately absent: two
// actions over the same bytes in the same toolchain share one key.
func ActionPreimage(a Action) ([]byte, error) {
	normalized, err := NormalizeAction(a)
	if err != nil {
		return nil, err
	}
	inputs := make([]wcb.Value, len(normalized.Inputs))
	for i, in := range normalized.Inputs {
		inputs[i] = wcb.TextValue(in.ContentID)
	}
	params := make(map[string]wcb.Value, len(normalized.Params))
	for key, value := range normalized.Params {
		params[key] = wcb.TextValue(value)
	}
	value := wcb.MapValue(map[string]wcb.Value{
		"schema":      wcb.TextValue(ActionSchema),
		"action_type": wcb.TextValue(normalized.ActionType),
		"toolchain":   wcb.TextValue(normalized.Toolchain),
		"inputs":      wcb.ArrayValue(inputs),
		"params":      wcb.MapValue(params),
	})
	return wcb.Encode(value, limits.Default())
}

// DeriveKey computes the canonical action identity and the deterministic
// ObjectIds and selector that follow from it.
func DeriveKey(a Action) (Key, error) {
	preimage, err := ActionPreimage(a)
	if err != nil {
		return Key{}, err
	}
	actionKey := hash.Domain(KeyDomain, preimage).Hex()
	return keyFromDigest(actionKey, preimage)
}

func keyFromDigest(actionKey string, preimage []byte) (Key, error) {
	recordID := ObjectIDForSeed(RecordSeedPrefix + actionKey)
	outputID := ObjectIDForSeed(OutputSeedPrefix + actionKey)
	selectorText := ContextSelector(recordID.Hex())
	return Key{
		ActionKey:       actionKey,
		ActionPreimage:  hex.EncodeToString(preimage),
		RecordObjectID:  recordID.Hex(),
		OutputObjectID:  outputID.Hex(),
		ContextSelector: selectorText,
	}, nil
}

// ObjectIDForSeed computes ObjectId = H("weft/object/v1", seed), matching the
// frozen WeftFS identity construction in both languages.
func ObjectIDForSeed(seed string) hash.Hash32 {
	return hash.Domain(hash.DomainObject, []byte(seed))
}

// ContextSelector is the canonical WeftQL text that selects exactly the output
// of one action record via the reverse `cas_output_of` edge.
func ContextSelector(recordObjectID string) string {
	return fmt.Sprintf("select relation(%s, obj:%s)", PredicateOutputOf, recordObjectID)
}

// ParseKey accepts a 64-hex action key and returns the derived key material.
func ParseKey(actionKey string) (Key, error) {
	normalized, err := validateContentID(actionKey)
	if err != nil {
		return Key{}, fmt.Errorf("action key: %w", err)
	}
	if normalized != actionKey {
		return Key{}, fmt.Errorf("action key %q must be lowercase hex", actionKey)
	}
	return keyFromDigest(normalized, nil)
}

// RandomSeed returns a 32-byte seed for tests that need a non-deterministic
// store-created object (the CAS itself always uses deterministic seeds).
func RandomSeed() ([]byte, error) {
	seed := make([]byte, 32)
	if _, err := rand.Read(seed); err != nil {
		return nil, err
	}
	return seed, nil
}
