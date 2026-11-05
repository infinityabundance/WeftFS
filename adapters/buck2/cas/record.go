package cas

import (
	"bytes"
	"fmt"
	"sort"

	"github.com/infinityabundance/WeftCodec-Go/limits"
	"github.com/infinityabundance/WeftCodec-Go/wcb"
)

// Record is the content-addressed action-cache record. It is stored as a
// canonical WCB file at /cas/records/<action-key>. The file object's ContentId
// is H("weft/chunk/v1", record bytes); its ObjectId is deterministic.
type Record struct {
	Schema          string            `json:"schema"`
	ActionKey       string            `json:"action_key"`
	ActionType      string            `json:"action_type"`
	Toolchain       string            `json:"toolchain"`
	Inputs          []Input           `json:"inputs"`
	Params          map[string]string `json:"params,omitempty"`
	OutputPath      string            `json:"output_path"`
	OutputContentID string            `json:"output_content_id"`
	OutputObjectID  string            `json:"output_object_id"`
	OutputBytes     int64             `json:"output_bytes"`
}

// NewRecord builds the record for a key and output.
func NewRecord(key Key, action Action, outputPath string, output []byte, outputObjectID string) (Record, error) {
	normalized, err := NormalizeAction(action)
	if err != nil {
		return Record{}, err
	}
	cleanOut, err := ValidateOutputPath(outputPath)
	if err != nil {
		return Record{}, err
	}
	if len(output) > MaxOutputBytes {
		return Record{}, fmt.Errorf("output is %d bytes, above the %d-byte bound", len(output), MaxOutputBytes)
	}
	return Record{
		Schema:          RecordSchema,
		ActionKey:       key.ActionKey,
		ActionType:      normalized.ActionType,
		Toolchain:       normalized.Toolchain,
		Inputs:          normalized.Inputs,
		Params:          normalized.Params,
		OutputPath:      cleanOut,
		OutputContentID: ContentID(output),
		OutputObjectID:  outputObjectID,
		OutputBytes:     int64(len(output)),
	}, nil
}

func inputValue(in Input) wcb.Value {
	return wcb.MapValue(map[string]wcb.Value{
		"path":       wcb.TextValue(in.Path),
		"content_id": wcb.TextValue(in.ContentID),
		"bytes":      wcb.U64Value(uint64(in.Bytes)),
	})
}

// RecordBytes encodes a record as canonical WCB.
func RecordBytes(r Record) ([]byte, error) {
	inputs := make([]wcb.Value, len(r.Inputs))
	for i, in := range r.Inputs {
		inputs[i] = inputValue(in)
	}
	params := make(map[string]wcb.Value, len(r.Params))
	for key, value := range r.Params {
		params[key] = wcb.TextValue(value)
	}
	value := wcb.MapValue(map[string]wcb.Value{
		"schema":            wcb.TextValue(RecordSchema),
		"action_key":        wcb.TextValue(r.ActionKey),
		"action_type":       wcb.TextValue(r.ActionType),
		"toolchain":         wcb.TextValue(r.Toolchain),
		"inputs":            wcb.ArrayValue(inputs),
		"params":            wcb.MapValue(params),
		"output_path":       wcb.TextValue(r.OutputPath),
		"output_content_id": wcb.TextValue(r.OutputContentID),
		"output_object_id":  wcb.TextValue(r.OutputObjectID),
		"output_bytes":      wcb.U64Value(uint64(r.OutputBytes)),
	})
	return wcb.Encode(value, limits.Default())
}

var recordKeys = map[string]bool{
	"schema":            true,
	"action_key":        true,
	"action_type":       true,
	"toolchain":         true,
	"inputs":            true,
	"params":            true,
	"output_path":       true,
	"output_content_id": true,
	"output_object_id":  true,
	"output_bytes":      true,
}

// DecodeRecord strictly decodes a canonical record and validates every field,
// including that the decoded bytes re-encode identically and that the inputs
// are in canonical order. Any deviation is an error (fail closed).
func DecodeRecord(encoded []byte) (Record, error) {
	if len(encoded) > MaxRecordBytes {
		return Record{}, fmt.Errorf("action record is %d bytes, above the %d-byte bound", len(encoded), MaxRecordBytes)
	}
	value, err := wcb.Decode(encoded, limits.Default())
	if err != nil {
		return Record{}, fmt.Errorf("action record is not canonical WCB: %w", err)
	}
	if value.Kind != wcb.Map || value.Map == nil {
		return Record{}, fmt.Errorf("action record must be a WCB map")
	}
	for key := range value.Map {
		if !recordKeys[key] {
			return Record{}, fmt.Errorf("action record has unknown field %q", key)
		}
	}
	for key := range recordKeys {
		if _, ok := value.Map[key]; !ok {
			return Record{}, fmt.Errorf("action record is missing field %q", key)
		}
	}
	schema, err := textField(value.Map, "schema")
	if err != nil {
		return Record{}, err
	}
	if schema != RecordSchema {
		return Record{}, fmt.Errorf("action record schema %q is not %q", schema, RecordSchema)
	}
	actionKey, err := textField(value.Map, "action_key")
	if err != nil {
		return Record{}, err
	}
	actionKey, err = validateContentID(actionKey)
	if err != nil {
		return Record{}, fmt.Errorf("action_key: %w", err)
	}
	actionType, err := textField(value.Map, "action_type")
	if err != nil {
		return Record{}, err
	}
	toolchain, err := textField(value.Map, "toolchain")
	if err != nil {
		return Record{}, err
	}
	outputPath, err := textField(value.Map, "output_path")
	if err != nil {
		return Record{}, err
	}
	outputPath, err = ValidateOutputPath(outputPath)
	if err != nil {
		return Record{}, err
	}
	outputContentID, err := textField(value.Map, "output_content_id")
	if err != nil {
		return Record{}, err
	}
	outputContentID, err = validateContentID(outputContentID)
	if err != nil {
		return Record{}, fmt.Errorf("output_content_id: %w", err)
	}
	outputObjectID, err := textField(value.Map, "output_object_id")
	if err != nil {
		return Record{}, err
	}
	outputObjectID, err = validateContentID(outputObjectID)
	if err != nil {
		return Record{}, fmt.Errorf("output_object_id: %w", err)
	}
	outputBytes, err := u64Field(value.Map, "output_bytes")
	if err != nil {
		return Record{}, err
	}
	if outputBytes > MaxOutputBytes {
		return Record{}, fmt.Errorf("action record output_bytes %d is above the %d-byte bound", outputBytes, MaxOutputBytes)
	}
	inputs, err := decodeInputs(value.Map["inputs"])
	if err != nil {
		return Record{}, err
	}
	params, err := decodeParams(value.Map["params"])
	if err != nil {
		return Record{}, err
	}
	record := Record{
		Schema:          schema,
		ActionKey:       actionKey,
		ActionType:      actionType,
		Toolchain:       toolchain,
		Inputs:          inputs,
		Params:          params,
		OutputPath:      outputPath,
		OutputContentID: outputContentID,
		OutputObjectID:  outputObjectID,
		OutputBytes:     int64(outputBytes),
	}
	// Canonical array order is part of the record contract; WCB alone does not
	// order arrays.
	if !sort.SliceIsSorted(inputs, func(i, j int) bool {
		if inputs[i].ContentID != inputs[j].ContentID {
			return inputs[i].ContentID < inputs[j].ContentID
		}
		return inputs[i].Path < inputs[j].Path
	}) {
		return Record{}, fmt.Errorf("action record inputs are not in canonical order")
	}
	reencoded, err := RecordBytes(record)
	if err != nil {
		return Record{}, err
	}
	if !bytes.Equal(reencoded, encoded) {
		return Record{}, fmt.Errorf("action record bytes are not canonical (re-encoding differs)")
	}
	return record, nil
}

func decodeInputs(value wcb.Value) ([]Input, error) {
	if value.Kind != wcb.Array {
		return nil, fmt.Errorf("action record inputs must be an array")
	}
	if len(value.Array) > MaxInputs {
		return nil, fmt.Errorf("action record has %d inputs, above the %d-input bound", len(value.Array), MaxInputs)
	}
	seen := make(map[string]bool, len(value.Array))
	inputs := make([]Input, 0, len(value.Array))
	for index, item := range value.Array {
		if item.Kind != wcb.Map {
			return nil, fmt.Errorf("action record input %d must be a map", index)
		}
		path, err := textField(item.Map, "path")
		if err != nil {
			return nil, fmt.Errorf("input %d: %w", index, err)
		}
		path, err = validateInputPath(path)
		if err != nil {
			return nil, err
		}
		if seen[path] {
			return nil, fmt.Errorf("duplicate input path %q", path)
		}
		seen[path] = true
		contentID, err := textField(item.Map, "content_id")
		if err != nil {
			return nil, fmt.Errorf("input %d: %w", index, err)
		}
		contentID, err = validateContentID(contentID)
		if err != nil {
			return nil, fmt.Errorf("input %d: %w", index, err)
		}
		bytes, err := u64Field(item.Map, "bytes")
		if err != nil {
			return nil, fmt.Errorf("input %d: %w", index, err)
		}
		inputs = append(inputs, Input{Path: path, ContentID: contentID, Bytes: int64(bytes)})
	}
	return inputs, nil
}

func decodeParams(value wcb.Value) (map[string]string, error) {
	if value.Kind != wcb.Map {
		return nil, fmt.Errorf("action record params must be a map")
	}
	if len(value.Map) > MaxParams {
		return nil, fmt.Errorf("action record has %d params, above the %d-param bound", len(value.Map), MaxParams)
	}
	params := make(map[string]string, len(value.Map))
	for key, item := range value.Map {
		if key == "" {
			return nil, fmt.Errorf("action record param name must not be empty")
		}
		if item.Kind != wcb.Text {
			return nil, fmt.Errorf("action record param %q must be text", key)
		}
		params[key] = item.Str
	}
	return params, nil
}

func textField(m map[string]wcb.Value, key string) (string, error) {
	value, ok := m[key]
	if !ok {
		return "", fmt.Errorf("missing field %q", key)
	}
	if value.Kind != wcb.Text {
		return "", fmt.Errorf("field %q must be text", key)
	}
	return value.Str, nil
}

func u64Field(m map[string]wcb.Value, key string) (uint64, error) {
	value, ok := m[key]
	if !ok {
		return 0, fmt.Errorf("missing field %q", key)
	}
	if value.Kind != wcb.U64 {
		return 0, fmt.Errorf("field %q must be u64", key)
	}
	return value.U, nil
}

// SortInputs is exported for tests that compare canonical ordering.
func SortInputs(inputs []Input) {
	sort.Slice(inputs, func(i, j int) bool {
		if inputs[i].ContentID != inputs[j].ContentID {
			return inputs[i].ContentID < inputs[j].ContentID
		}
		return inputs[i].Path < inputs[j].Path
	})
}
