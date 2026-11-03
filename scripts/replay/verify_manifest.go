// Independent Go verification for a WeftFS replay bundle.
//
// It never runs Rust. It reads `manifest.json`, re-hashes every store entry,
// re-hashes every materialized Context Root file, and compares the Go
// frontend's resolved ContextRootId/SnapshotId/SelectorId (written beside the
// bundle as `resolve-<name>.json`) with the manifest.
//
// Usage: verify_manifest BUNDLE_DIR MATERIALIZED_DIR
package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
)

type StoreEntry struct {
	Path   string `json:"path"`
	SHA256 string `json:"sha256"`
	Bytes  int64  `json:"bytes"`
}

type ContextFile struct {
	Path   string `json:"path"`
	SHA256 string `json:"sha256"`
	Size   int64  `json:"size"`
	Kind   string `json:"kind"`
}

type ContextRecord struct {
	Name          string        `json:"name"`
	ContextRootID string        `json:"context_root_id"`
	SnapshotID    string        `json:"snapshot_id"`
	SelectorID    string        `json:"selector_id"`
	Files         []ContextFile `json:"files"`
}

type Manifest struct {
	Schema       string          `json:"schema"`
	Store        map[string]any  `json:"store"`
	Contexts     []ContextRecord `json:"contexts"`
	StoreEntries []StoreEntry    `json:"store_entries"`
	SourceRev    map[string]any  `json:"source_revisions"`
}

type Summary struct {
	OK              bool     `json:"ok"`
	Schema          string   `json:"schema"`
	StoreEntries    int      `json:"store_entries_checked"`
	Contexts        int      `json:"contexts_checked"`
	ContextFiles    int      `json:"context_files_checked"`
	ResolvedIDs     int      `json:"resolved_ids_checked"`
	Mismatches      []string `json:"mismatches"`
	MaterializedDir string   `json:"materialized_dir"`
}

func sha256File(path string) (string, int64, error) {
	handle, err := os.Open(path)
	if err != nil {
		return "", 0, err
	}
	defer handle.Close()
	hasher := sha256.New()
	bytes, err := io.Copy(hasher, handle)
	if err != nil {
		return "", 0, err
	}
	return hex.EncodeToString(hasher.Sum(nil)), bytes, nil
}

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: verify_manifest BUNDLE_DIR MATERIALIZED_DIR")
		os.Exit(64)
	}
	bundleDir := os.Args[1]
	materializedDir := os.Args[2]
	raw, err := os.ReadFile(filepath.Join(bundleDir, "manifest.json"))
	if err != nil {
		fmt.Fprintln(os.Stderr, "verify_manifest:", err)
		os.Exit(66)
	}
	var manifest Manifest
	if err := json.Unmarshal(raw, &manifest); err != nil {
		fmt.Fprintln(os.Stderr, "verify_manifest: manifest:", err)
		os.Exit(65)
	}
	summary := Summary{Schema: manifest.Schema, Mismatches: []string{}, MaterializedDir: materializedDir}
	for _, entry := range manifest.StoreEntries {
		summary.StoreEntries++
		got, size, err := sha256File(filepath.Join(bundleDir, entry.Path))
		if err != nil {
			summary.Mismatches = append(summary.Mismatches, entry.Path+": "+err.Error())
			continue
		}
		if got != entry.SHA256 {
			summary.Mismatches = append(summary.Mismatches, fmt.Sprintf("%s: sha256 %s != %s", entry.Path, got, entry.SHA256))
		}
		if entry.Bytes != 0 && size != entry.Bytes {
			summary.Mismatches = append(summary.Mismatches, fmt.Sprintf("%s: size %d != %d", entry.Path, size, entry.Bytes))
		}
	}
	for _, context := range manifest.Contexts {
		summary.Contexts++
		for _, file := range context.Files {
			if file.SHA256 == "" || file.Kind == "dir" || file.Kind == "symlink" {
				continue
			}
			summary.ContextFiles++
			relative := strings.TrimPrefix(file.Path, "/")
			got, size, err := sha256File(filepath.Join(materializedDir, context.Name, relative))
			if err != nil {
				summary.Mismatches = append(summary.Mismatches, context.Name+"/"+relative+": "+err.Error())
				continue
			}
			if got != file.SHA256 {
				summary.Mismatches = append(summary.Mismatches, fmt.Sprintf("%s/%s: sha256 %s != %s", context.Name, relative, got, file.SHA256))
			}
			if file.Size != 0 && size != file.Size {
				summary.Mismatches = append(summary.Mismatches, fmt.Sprintf("%s/%s: size %d != %d", context.Name, relative, size, file.Size))
			}
		}
		// The Go resolver must reproduce the manifest's identities.
		resolvePath := filepath.Join(bundleDir, "resolve-"+context.Name+".json")
		resolveRaw, err := os.ReadFile(resolvePath)
		if err != nil {
			summary.Mismatches = append(summary.Mismatches, context.Name+": missing "+resolvePath)
			continue
		}
		var resolved map[string]any
		if err := json.Unmarshal(resolveRaw, &resolved); err != nil {
			summary.Mismatches = append(summary.Mismatches, context.Name+": bad resolve JSON")
			continue
		}
		for field, expected := range map[string]string{
			"context_root_id": context.ContextRootID,
			"snapshot_id":     context.SnapshotID,
			"selector_id":     context.SelectorID,
		} {
			actual, _ := resolved[field].(string)
			if actual == "" {
				summary.Mismatches = append(summary.Mismatches, context.Name+": resolve JSON lacks "+field)
				continue
			}
			if actual != expected {
				summary.Mismatches = append(summary.Mismatches, fmt.Sprintf("%s: %s %s != %s", context.Name, field, actual, expected))
			}
		}
		summary.ResolvedIDs++
	}
	summary.OK = len(summary.Mismatches) == 0
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(summary); err != nil {
		fmt.Fprintln(os.Stderr, "verify_manifest:", err)
		os.Exit(65)
	}
	if !summary.OK {
		os.Exit(1)
	}
}
