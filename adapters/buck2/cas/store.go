package cas

import (
	"bytes"
	"fmt"
	"io/fs"
	"os"
	"path"
	"path/filepath"
	"sort"
	"strings"

	weftcontext "github.com/infinityabundance/WeftCodec-Go/context"
	"github.com/infinityabundance/WeftCodec-Go/hash"
	"github.com/infinityabundance/WeftCodec-Go/limits"
	"github.com/infinityabundance/WeftCodec-Go/selector"
	"github.com/infinityabundance/WeftCodec-Go/wcb"
	"github.com/infinityabundance/WeftFS-Go/auth"
	"github.com/infinityabundance/WeftFS-Go/model"
	"github.com/infinityabundance/WeftFS-Go/store"
	"github.com/infinityabundance/WeftFS-Go/werr"
)

// Store is an open WeftFS store used as the CAS substrate.
type Store struct {
	s    *store.Store
	mode store.OpenMode
}

// Init creates a new WeftFS store at root and opens it for writing.
func Init(root string, opts store.Options) (*Store, error) {
	s, err := store.Init(root, opts)
	if err != nil {
		return nil, err
	}
	return &Store{s: s, mode: store.ReadWrite}, nil
}

// Open opens an existing store for writing (exclusive writer lock).
func Open(root string) (*Store, error) {
	s, err := store.Open(root, store.ReadWrite)
	if err != nil {
		return nil, err
	}
	return &Store{s: s, mode: store.ReadWrite}, nil
}

// OpenReadOnly opens an existing store without taking the writer lock.
func OpenReadOnly(root string) (*Store, error) {
	s, err := store.Open(root, store.ReadOnly)
	if err != nil {
		return nil, err
	}
	return &Store{s: s, mode: store.ReadOnly}, nil
}

// Close releases the store.
func (c *Store) Close() error {
	if c == nil || c.s == nil {
		return nil
	}
	return c.s.Close()
}

// Root is the store directory.
func (c *Store) Root() string { return c.s.Root() }

// Head is the current commit root hash as lowercase hex.
func (c *Store) Head() string { return c.s.Head().Hex() }

// Generation is the published commit generation.
func (c *Store) Generation() uint64 { return c.s.Generation() }

// SetGCGrace overrides the GC grace window for this open store.
func (c *Store) SetGCGrace(seconds uint64) error {
	opts := c.s.Options()
	opts.GCGraceSecs = seconds
	return c.s.SetOptions(opts)
}

// RecordPath is the in-store path of an action record.
func RecordPath(actionKey string) string { return path.Join(RecordsDir, actionKey) }

// OutputDir is the in-store directory holding one action's output.
func OutputDir(actionKey string) string { return path.Join(OutputsDir, actionKey) }

// OutputPath is the in-store path of one action's output blob.
func OutputPath(actionKey, declaredPath string) string {
	return path.Join(OutputDir(actionKey), declaredPath)
}

func (c *Store) resolve(storePath string) (model.Hash32, error) {
	return c.s.ResolvePath(storePath)
}

func (c *Store) read(storePath string) (model.Hash32, []byte, error) {
	id, err := c.resolve(storePath)
	if err != nil {
		return model.Hash32{}, nil, err
	}
	data, err := c.s.ReadObject(id)
	if err != nil {
		return model.Hash32{}, nil, err
	}
	return id, data, nil
}

func (c *Store) ensureDir(storePath string) (model.Hash32, error) {
	clean := path.Clean("/" + strings.TrimPrefix(storePath, "/"))
	if clean == "/" {
		return model.RootID, nil
	}
	parts := strings.Split(strings.TrimPrefix(clean, "/"), "/")
	parent := model.RootID
	current := ""
	for _, part := range parts {
		current += "/" + part
		id, err := c.s.ResolvePath(current)
		if err == nil {
			parent = id
			continue
		}
		if !werr.Is(err, werr.NotFound) {
			return model.Hash32{}, err
		}
		seed := DirSeedPrefix + current
		id, err = c.s.MkdirSeeded(parent, []byte(part), 0o755, 0, 0, []byte(seed))
		if err != nil {
			return model.Hash32{}, err
		}
		parent = id
	}
	return parent, nil
}

// createFile publishes data at storePath with a deterministic ObjectId. If the
// path already exists with exactly the same bytes it is reused; different bytes
// at the same path fail closed.
func (c *Store) createFile(storePath string, data []byte, seed string, attrs map[string]wcb.Value) (model.Hash32, error) {
	if id, existing, err := c.read(storePath); err == nil {
		if bytes.Equal(existing, data) {
			return id, nil
		}
		return model.Hash32{}, werr.Newf(werr.Conflict,
			"%s already holds different bytes (content %s, want %s)",
			storePath, ContentID(existing), ContentID(data))
	} else if !werr.Is(err, werr.NotFound) {
		return model.Hash32{}, err
	}
	parentPath, name := path.Dir(storePath), path.Base(storePath)
	parent, err := c.ensureDir(parentPath)
	if err != nil {
		return model.Hash32{}, err
	}
	id, err := c.s.CreateFileSeeded(parent, []byte(name), data, 0o644, 0, 0, attrs, []byte(seed))
	if err != nil {
		return model.Hash32{}, err
	}
	if want := ObjectIDForSeed(seed); id != want {
		return model.Hash32{}, werr.Newf(werr.Corrupt,
			"%s ObjectId %s does not match its seed (%s)", storePath, id.Hex(), want.Hex())
	}
	return id, nil
}

// loadRecord reads and validates the stored action record for actionKey.
func (c *Store) loadRecord(actionKey string) (Record, model.Hash32, []byte, error) {
	key, err := ParseKey(actionKey)
	if err != nil {
		return Record{}, model.Hash32{}, nil, err
	}
	id, encoded, err := c.read(RecordPath(key.ActionKey))
	if err != nil {
		if werr.Is(err, werr.NotFound) {
			return Record{}, model.Hash32{}, nil, ErrMiss
		}
		return Record{}, model.Hash32{}, nil, err
	}
	record, err := DecodeRecord(encoded)
	if err != nil {
		return Record{}, model.Hash32{}, nil, werr.Newf(werr.Corrupt,
			"action record %s: %v", key.ActionKey, err)
	}
	if record.ActionKey != key.ActionKey {
		return Record{}, model.Hash32{}, nil, werr.Newf(werr.Corrupt,
			"action record %s declares key %s", RecordPath(key.ActionKey), record.ActionKey)
	}
	rederived, err := DeriveKey(Action{
		ActionType: record.ActionType,
		Toolchain:  record.Toolchain,
		Inputs:     record.Inputs,
		Params:     record.Params,
	})
	if err != nil {
		return Record{}, model.Hash32{}, nil, werr.Newf(werr.Corrupt,
			"action record %s cannot be re-derived: %v", key.ActionKey, err)
	}
	if rederived.ActionKey != key.ActionKey {
		return Record{}, model.Hash32{}, nil, werr.Newf(werr.Corrupt,
			"action record %s re-derives as %s", key.ActionKey, rederived.ActionKey)
	}
	if id != ObjectIDForSeed(RecordSeedPrefix+key.ActionKey) {
		return Record{}, model.Hash32{}, nil, werr.Newf(werr.Corrupt,
			"action record %s has ObjectId %s, want the deterministic record id",
			RecordPath(key.ActionKey), id.Hex())
	}
	return record, id, encoded, nil
}

// readOutput reads and verifies the recorded output blob.
func (c *Store) readOutput(record Record) (model.Hash32, []byte, error) {
	storePath := OutputPath(record.ActionKey, record.OutputPath)
	id, data, err := c.read(storePath)
	if err != nil {
		if werr.Is(err, werr.NotFound) {
			return model.Hash32{}, nil, werr.Newf(werr.Corrupt,
				"action %s output %s is missing", record.ActionKey, storePath)
		}
		return model.Hash32{}, nil, err
	}
	if int64(len(data)) != record.OutputBytes {
		return model.Hash32{}, nil, werr.Newf(werr.Corrupt,
			"action %s output is %d bytes, record declares %d",
			record.ActionKey, len(data), record.OutputBytes)
	}
	if got := ContentID(data); got != record.OutputContentID {
		return model.Hash32{}, nil, werr.Newf(werr.Corrupt,
			"action %s output content id is %s, record declares %s",
			record.ActionKey, got, record.OutputContentID)
	}
	if id.Hex() != record.OutputObjectID {
		return model.Hash32{}, nil, werr.Newf(werr.Corrupt,
			"action %s output ObjectId is %s, record declares %s",
			record.ActionKey, id.Hex(), record.OutputObjectID)
	}
	return id, data, nil
}

// ErrMiss is returned when no action record exists for the key.
var ErrMiss = fmt.Errorf("action cache miss")

// StoreAction publishes an output blob and its action record. It is idempotent
// for the same key and identical bytes. A same-key/different-bytes request
// fails closed before any store mutation.
func (c *Store) StoreAction(a Action, output []byte, outputPath string) (Record, bool, error) {
	if c.mode != store.ReadWrite {
		return Record{}, false, werr.Newf(werr.ReadOnly, "store is open read-only")
	}
	normalized, err := NormalizeAction(a)
	if err != nil {
		return Record{}, false, err
	}
	key, err := DeriveKey(normalized)
	if err != nil {
		return Record{}, false, err
	}
	cleanOut, err := ValidateOutputPath(outputPath)
	if err != nil {
		return Record{}, false, err
	}
	if len(output) > MaxOutputBytes {
		return Record{}, false, fmt.Errorf("output is %d bytes, above the %d-byte bound", len(output), MaxOutputBytes)
	}
	outputContent := ContentID(output)

	// Conflict detection precedes every mutation.
	if existing, _, _, err := c.loadRecord(key.ActionKey); err == nil {
		if existing.OutputContentID != outputContent {
			return Record{}, false, werr.Newf(werr.Conflict,
				"same action key %s has different output bytes (stored %s, received %s)",
				key.ActionKey, existing.OutputContentID, outputContent)
		}
		if _, _, err := c.readOutput(existing); err != nil {
			return Record{}, false, err
		}
		return existing, true, nil
	} else if err != ErrMiss {
		return Record{}, false, err
	}

	attrs := map[string]wcb.Value{
		AttrActionKey:    wcb.TextValue(key.ActionKey),
		AttrContentID:    wcb.TextValue(outputContent),
		AttrOutputPath:   wcb.TextValue(cleanOut),
		AttrActionRecord: wcb.TextValue(key.RecordObjectID),
	}
	outputObjectID, err := c.createFile(
		OutputPath(key.ActionKey, cleanOut), output, OutputSeedPrefix+key.ActionKey, attrs)
	if err != nil {
		return Record{}, false, err
	}
	record, err := NewRecord(key, normalized, cleanOut, output, outputObjectID.Hex())
	if err != nil {
		return Record{}, false, err
	}
	encoded, err := RecordBytes(record)
	if err != nil {
		return Record{}, false, err
	}
	recordObjectID, err := c.createFile(
		RecordPath(key.ActionKey), encoded, RecordSeedPrefix+key.ActionKey, nil)
	if err != nil {
		return Record{}, false, err
	}
	if recordOutputID, err := hash.FromHex(record.OutputObjectID); err != nil || recordOutputID != outputObjectID {
		return Record{}, false, werr.Newf(werr.Corrupt, "output object id changed while publishing")
	}
	if _, err := c.s.RelationAdd(recordObjectID, PredicateAction,
		model.ObjectTarget(outputObjectID), nil, nil); err != nil {
		return Record{}, false, err
	}
	if _, err := c.s.RelationAdd(outputObjectID, PredicateOutputOf,
		model.ObjectTarget(recordObjectID), nil, nil); err != nil {
		return Record{}, false, err
	}
	return record, false, nil
}

// Fetch materializes the output of an action to outFile.
func (c *Store) Fetch(actionKey, outFile string) (Record, []byte, error) {
	record, _, _, err := c.loadRecord(actionKey)
	if err != nil {
		return Record{}, nil, err
	}
	_, data, err := c.readOutput(record)
	if err != nil {
		return Record{}, nil, err
	}
	if outFile != "" {
		if err := writeFileAtomically(outFile, data); err != nil {
			return Record{}, nil, err
		}
	}
	return record, data, nil
}

// QueryResult is the resolved output of one action.
type QueryResult struct {
	ActionKey      string `json:"action_key"`
	RecordObjectID string `json:"record_object_id"`
	OutputObjectID string `json:"output_object_id"`
	ContentID      string `json:"content_id"`
	StorePath      string `json:"store_path"`
	OutputPath     string `json:"output_path"`
	Bytes          int64  `json:"bytes"`
	Selector       string `json:"selector"`
}

// QueryByAction resolves by:action=<key> through the store's selector engine.
func (c *Store) QueryByAction(actionKey string) (QueryResult, error) {
	record, recordObjectID, _, err := c.loadRecord(actionKey)
	if err != nil {
		return QueryResult{}, err
	}
	selectorText := ContextSelector(recordObjectID.Hex())
	parsed, err := selector.ParseWith(selectorText, Namespace, limits.Default())
	if err != nil {
		return QueryResult{}, err
	}
	resolution, err := c.s.Resolve(parsed, nil, auth.Root(), limits.Default())
	if err != nil {
		return QueryResult{}, err
	}
	if resolution.MatchedCount != 1 || len(resolution.Results) != 1 {
		return QueryResult{}, werr.Newf(werr.Ambiguous,
			"by:action=%s matched %d objects; exactly 1 is required", actionKey, resolution.MatchedCount)
	}
	result := resolution.Results[0]
	if result.ObjectID.Hex() != record.OutputObjectID {
		return QueryResult{}, werr.Newf(werr.Corrupt,
			"selector resolved object %s, record declares %s", result.ObjectID.Hex(), record.OutputObjectID)
	}
	_, data, err := c.readOutput(record)
	if err != nil {
		return QueryResult{}, err
	}
	return QueryResult{
		ActionKey:      actionKey,
		RecordObjectID: recordObjectID.Hex(),
		OutputObjectID: result.ObjectID.Hex(),
		ContentID:      ContentID(data),
		StorePath:      OutputPath(record.ActionKey, record.OutputPath),
		OutputPath:     record.OutputPath,
		Bytes:          int64(len(data)),
		Selector:       selectorText,
	}, nil
}

// Verify re-hashes the stored output and, when supplied, an external blob. It
// re-derives the action key from the stored record as well.
func (c *Store) Verify(actionKey string, blob []byte) (Record, error) {
	record, _, _, err := c.loadRecord(actionKey)
	if err != nil {
		return Record{}, err
	}
	_, stored, err := c.readOutput(record)
	if err != nil {
		return Record{}, err
	}
	if blob != nil {
		if got := ContentID(blob); got != record.OutputContentID {
			return Record{}, werr.Newf(werr.Corrupt,
				"supplied blob content id is %s, record declares %s", got, record.OutputContentID)
		}
		if !bytes.Equal(blob, stored) {
			return Record{}, werr.Newf(werr.Corrupt, "supplied blob differs from the stored output")
		}
	}
	return record, nil
}

// VerifyAll verifies every stored action record.
func (c *Store) VerifyAll() (int, error) {
	parent, err := c.resolve(RecordsDir)
	if err != nil {
		if werr.Is(err, werr.NotFound) {
			return 0, nil
		}
		return 0, err
	}
	entries, err := c.s.ListDir(parent)
	if err != nil {
		return 0, err
	}
	names := make([]string, 0, len(entries))
	for _, entry := range entries {
		names = append(names, string(entry.Name))
	}
	sort.Strings(names)
	for _, name := range names {
		if _, _, _, err := c.loadRecord(name); err != nil {
			return 0, fmt.Errorf("record %s: %w", name, err)
		}
		if _, err := c.Verify(name, nil); err != nil {
			return 0, fmt.Errorf("record %s: %w", name, err)
		}
	}
	return len(names), nil
}

// ContextInfo describes a created Context Root.
type ContextInfo struct {
	Name           string `json:"name"`
	ContextRootID  string `json:"context_root_id"`
	URI            string `json:"uri"`
	Selector       string `json:"selector"`
	SnapshotID     string `json:"snapshot_id"`
	RecordObjectID string `json:"record_object_id"`
	OutputObjectID string `json:"output_object_id"`
	ContentID      string `json:"content_id"`
}

// CreateContext persists `by:action=<key>` as a Context Root. The selector is
// the one-hop `cas_output_of` relation, so resolution is exact and
// `--require-one` has real cardinality semantics.
func (c *Store) CreateContext(actionKey, name, snapshotName string, replace bool) (ContextInfo, error) {
	if c.mode != store.ReadWrite {
		return ContextInfo{}, werr.Newf(werr.ReadOnly, "store is open read-only")
	}
	record, recordObjectID, _, err := c.loadRecord(actionKey)
	if err != nil {
		return ContextInfo{}, err
	}
	if name == "" {
		name = "by:action=" + record.ActionKey
	}
	selectorText := ContextSelector(recordObjectID.Hex())
	parsed, err := selector.ParseWith(selectorText, Namespace, limits.Default())
	if err != nil {
		return ContextInfo{}, err
	}
	snapshot := c.s.Head()
	if snapshotName != "" {
		snapshot, err = c.s.LookupSnapshot(snapshotName)
		if err != nil {
			return ContextInfo{}, err
		}
	}
	resolved, err := c.s.RequireOne(parsed, &snapshot, auth.Root(), limits.Default())
	if err != nil {
		return ContextInfo{}, err
	}
	root, err := weftcontext.NewContextRoot(
		parsed, snapshot, weftcontext.DefaultResolver, nil, nil, nil)
	if err != nil {
		return ContextInfo{}, err
	}
	if _, err := c.s.SaveContextRoot(name, root, nil, replace); err != nil {
		return ContextInfo{}, err
	}
	stored, err := c.s.LoadContextRoot(name)
	if err != nil {
		return ContextInfo{}, err
	}
	_, data, err := c.readOutput(record)
	if err != nil {
		return ContextInfo{}, err
	}
	return ContextInfo{
		Name:           name,
		ContextRootID:  stored.ContextRootID.Hex(),
		URI:            stored.URI,
		Selector:       selectorText,
		SnapshotID:     stored.Root.Snapshot.Hex(),
		RecordObjectID: recordObjectID.Hex(),
		OutputObjectID: resolved.ObjectID.Hex(),
		ContentID:      ContentID(data),
	}, nil
}

// ResolveContextValue resolves a stored Context Root and returns its result
// object ids and content ids.
type ContextValue struct {
	Name          string   `json:"name"`
	ContextRootID string   `json:"context_root_id"`
	SnapshotID    string   `json:"snapshot_id"`
	MatchedCount  uint64   `json:"matched_count"`
	ObjectIDs     []string `json:"object_ids"`
	ContentIDs    []string `json:"content_ids"`
}

// ResolveContext resolves a Context Root, optionally requiring exactly one.
func (c *Store) ResolveContext(name string, requireOne bool) (ContextValue, error) {
	resolved, err := c.s.ResolveContext(name, nil, auth.Root(), limits.Default(), c.s.ClockNowNS())
	if err != nil {
		return ContextValue{}, err
	}
	if requireOne && resolved.Resolution.MatchedCount != 1 {
		return ContextValue{}, werr.Newf(werr.Ambiguous,
			"context %s matched %d objects; require_one needs exactly 1",
			name, resolved.Resolution.MatchedCount)
	}
	value := ContextValue{
		Name:          resolved.Stored.Name,
		ContextRootID: resolved.Stored.ContextRootID.Hex(),
		SnapshotID:    resolved.Stored.Root.Snapshot.Hex(),
		MatchedCount:  resolved.Resolution.MatchedCount,
	}
	for _, result := range resolved.Resolution.Results {
		value.ObjectIDs = append(value.ObjectIDs, result.ObjectID.Hex())
		value.ContentIDs = append(value.ContentIDs, result.ContentID.Hex())
	}
	return value, nil
}

// DeleteAction removes an action's relations and file objects from the live
// head. It does not touch blobs: the store's mark-and-sweep GC does that after
// the objects become unreachable.
func (c *Store) DeleteAction(actionKey string) error {
	if c.mode != store.ReadWrite {
		return werr.Newf(werr.ReadOnly, "store is open read-only")
	}
	record, recordObjectID, _, err := c.loadRecord(actionKey)
	if err != nil {
		return err
	}
	outputObjectID, err := hash.FromHex(record.OutputObjectID)
	if err != nil {
		return err
	}
	state := c.s.CurrentState()
	relationIDs := make([]model.Hash32, 0, 2)
	for id, relation := range state.Relations {
		switch {
		case relation.Subject == recordObjectID && relation.Predicate == PredicateAction:
			relationIDs = append(relationIDs, id)
		case relation.Subject == outputObjectID && relation.Predicate == PredicateOutputOf:
			relationIDs = append(relationIDs, id)
		}
	}
	sort.Slice(relationIDs, func(i, j int) bool { return relationIDs[i].Less(relationIDs[j]) })
	for _, id := range relationIDs {
		if err := c.s.RelationDelete(id); err != nil && !werr.Is(err, werr.NotFound) {
			return err
		}
	}
	for _, storePath := range []string{
		OutputPath(record.ActionKey, record.OutputPath),
		RecordPath(record.ActionKey),
	} {
		parentPath, name := path.Dir(storePath), path.Base(storePath)
		parent, err := c.resolve(parentPath)
		if err != nil {
			if werr.Is(err, werr.NotFound) {
				continue
			}
			return err
		}
		if err := c.s.Unlink(parent, []byte(name)); err != nil && !werr.Is(err, werr.NotFound) {
			return err
		}
	}
	return nil
}

// GCReport is a compact view of one mark-and-sweep pass.
type GCReport struct {
	Applied         bool     `json:"applied"`
	DeletedFiles    uint64   `json:"deleted_files"`
	DeletedBytes    uint64   `json:"deleted_bytes"`
	CandidateFiles  uint64   `json:"candidate_files"`
	CandidateBytes  uint64   `json:"candidate_bytes"`
	ReachableFiles  uint64   `json:"reachable_files"`
	ReachableBytes  uint64   `json:"reachable_bytes"`
	GraceRetained   uint64   `json:"grace_retained"`
	RemovedOrphans  []string `json:"removed_orphans,omitempty"`
	Problems        []string `json:"problems,omitempty"`
	DeletedSample   []string `json:"deleted_sample,omitempty"`
	CandidateSample []string `json:"candidate_sample,omitempty"`
	RemainingFiles  uint64   `json:"remaining_files"`
	RemainingBytes  uint64   `json:"remaining_bytes"`
}

// GC delegates to the store's mark-and-sweep collector.
func (c *Store) GC(apply bool, maxDeletes int) (GCReport, error) {
	report, err := c.s.GCBounded(apply, maxDeletes)
	if err != nil {
		return GCReport{}, err
	}
	out := GCReport{
		Applied:        report.Applied,
		DeletedFiles:   report.DeletedFiles,
		DeletedBytes:   report.DeletedBytes,
		CandidateFiles: report.UnreachableFiles,
		CandidateBytes: report.UnreachableBytes,
		ReachableFiles: report.ReachableFiles,
		ReachableBytes: report.ReachableBytes,
		GraceRetained:  report.GraceRetained,
		RemovedOrphans: report.RemovedOrphans,
		Problems:       report.Problems,
		RemainingFiles: report.RemainingFiles,
		RemainingBytes: report.RemainingBytes,
	}
	limit := 8
	if len(report.Deleted) < limit {
		limit = len(report.Deleted)
	}
	out.DeletedSample = append(out.DeletedSample, report.Deleted[:limit]...)
	limit = 8
	if len(report.Candidates) < limit {
		limit = len(report.Candidates)
	}
	out.CandidateSample = append(out.CandidateSample, report.Candidates[:limit]...)
	return out, nil
}

// PhysicalBytes summarizes on-disk bytes by top-level store directory.
type PhysicalBytes struct {
	Total   uint64            `json:"total"`
	ByDir   map[string]uint64 `json:"by_dir"`
	Files   uint64            `json:"files"`
	ByFiles map[string]uint64 `json:"files_by_dir"`
}

// PhysicalBytes walks the store and sums file sizes.
func (c *Store) PhysicalBytes() (PhysicalBytes, error) {
	out := PhysicalBytes{ByDir: map[string]uint64{}, ByFiles: map[string]uint64{}}
	root := c.s.Root()
	err := filepath.WalkDir(root, func(current string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.IsDir() {
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(root, current)
		if err != nil {
			return err
		}
		top := strings.Split(filepath.ToSlash(rel), "/")[0]
		size := uint64(info.Size())
		out.Total += size
		out.Files++
		out.ByDir[top] += size
		out.ByFiles[top]++
		return nil
	})
	return out, err
}

// ObjectMeta exposes the WeftFS object record for an object id.
func (c *Store) ObjectMeta(objectID string) (model.ObjectRecord, error) {
	id, err := hash.FromHex(objectID)
	if err != nil {
		return model.ObjectRecord{}, err
	}
	return c.s.ObjectMeta(id)
}

// ManifestPath is the on-disk path of an object's content manifest.
func (c *Store) ManifestPath(objectID string) (string, error) {
	record, err := c.ObjectMeta(objectID)
	if err != nil {
		return "", err
	}
	return store.BlobPath(c.s.Root(), store.BlobManifest, record.Content), nil
}

// ReadObjectAt reads an object in a pinned snapshot state.
func (c *Store) ReadObjectAt(snapshotHex, objectID string) ([]byte, error) {
	snapshot, err := hash.FromHex(snapshotHex)
	if err != nil {
		return nil, err
	}
	id, err := hash.FromHex(objectID)
	if err != nil {
		return nil, err
	}
	return c.s.ReadObjectAt(&snapshot, id)
}

// CurrentState returns the live metadata state (read-only use).
func (c *Store) CurrentState() *model.State { return c.s.CurrentState() }

// HeadModel is the current commit root.
func (c *Store) HeadModel() model.Hash32 { return c.s.Head() }

// Snapshot creates a named snapshot and returns its commit hash.
func (c *Store) Snapshot(name string) (string, error) {
	commit, err := c.s.SnapshotCreate(name)
	if err != nil {
		return "", err
	}
	return commit.Hex(), nil
}

func writeFileAtomically(target string, data []byte) error {
	dir := filepath.Dir(target)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(dir, ".cas-*")
	if err != nil {
		return err
	}
	name := tmp.Name()
	defer os.Remove(name)
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(name, target)
}

// SplitStorePath is used by tests and tools that need the parent/name pair.
func SplitStorePath(storePath string) (string, string) {
	return path.Dir(storePath), path.Base(storePath)
}
