// Command posix_torture is an fsx-style randomized POSIX operation harness.
//
// It mounts nothing itself: the court script mounts a WeftFS frontend and runs
// this program against the mountpoint. The program maintains an independent
// model of the namespace and file contents, performs randomized create/write/
// read/truncate/append/rename/rename-over/link/unlink/mkdir/rmdir/symlink/
// readlink/xattr/fsync/open-unlinked sequences, verifies the observed bytes and
// metadata after every operation, and writes a canonical manifest of the final
// tree. A second invocation with --verify-only checks that a different
// implementation mounted over the same store presents exactly that tree.
//
// It is stdlib-only so it can be built in the FUSE court without network access.
package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"math/rand"
	"os"
	"path"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"time"
)

const (
	kindFile    = "file"
	kindDir     = "dir"
	kindSymlink = "symlink"
	weftDir     = ".weft"
)

var errSkip = errors.New("operation not applicable")

type inode struct {
	kind    string
	content []byte
	mode    uint32
	nlink   uint32
	target  string
	xattrs  map[string][]byte
}

type model struct {
	entries map[string]*inode
	next    int
}

func newModel() *model {
	m := &model{entries: map[string]*inode{}}
	for index := 0; index < 4; index++ {
		name := fmt.Sprintf("d%d", index)
		m.entries[name] = &inode{kind: kindDir, mode: 0o755, nlink: 2, xattrs: map[string][]byte{}}
	}
	return m
}

func (m *model) add(rel string, node *inode) { m.entries[rel] = node }

func (m *model) remove(rel string) {
	node, ok := m.entries[rel]
	if !ok {
		return
	}
	delete(m.entries, rel)
	if node.kind != kindDir && node.nlink > 0 {
		node.nlink--
	}
}

func (m *model) dirs() []string {
	out := []string{""}
	for rel, node := range m.entries {
		if node.kind == kindDir {
			out = append(out, rel)
		}
	}
	sort.Strings(out)
	return out
}

func (m *model) files() []string {
	out := []string{}
	for rel, node := range m.entries {
		if node.kind == kindFile {
			out = append(out, rel)
		}
	}
	sort.Strings(out)
	return out
}

func (m *model) symlinks() []string {
	out := []string{}
	for rel, node := range m.entries {
		if node.kind == kindSymlink {
			out = append(out, rel)
		}
	}
	sort.Strings(out)
	return out
}

func (m *model) emptyDirs() []string {
	out := []string{}
	for rel, node := range m.entries {
		if node.kind != kindDir || !strings.HasPrefix(rel, "m") {
			continue
		}
		empty := true
		prefix := rel + "/"
		for other := range m.entries {
			if strings.HasPrefix(other, prefix) {
				empty = false
				break
			}
		}
		if empty {
			out = append(out, rel)
		}
	}
	sort.Strings(out)
	return out
}

func (m *model) children(rel string) []string {
	out := []string{}
	prefix := ""
	if rel != "" {
		prefix = rel + "/"
	}
	for other := range m.entries {
		if !strings.HasPrefix(other, prefix) || other == rel {
			continue
		}
		rest := strings.TrimPrefix(other, prefix)
		if !strings.Contains(rest, "/") {
			out = append(out, rest)
		}
	}
	sort.Strings(out)
	return out
}

func parent(rel string) string {
	dir := path.Dir(rel)
	if dir == "." {
		return ""
	}
	return dir
}

func join(dir, name string) string {
	if dir == "" {
		return name
	}
	return dir + "/" + name
}

func absPath(mount, rel string) string {
	if rel == "" {
		return mount
	}
	return filepath.Join(mount, filepath.FromSlash(rel))
}

func uniqueName(m *model, dir, prefix string, limit int) (string, bool) {
	for index := 0; index < limit; index++ {
		name := fmt.Sprintf("%s%d", prefix, index)
		if _, exists := m.entries[join(dir, name)]; !exists {
			return name, true
		}
	}
	return "", false
}

func randomBytes(rng *rand.Rand, size int) []byte {
	out := make([]byte, size)
	for index := range out {
		out[index] = byte(rng.Intn(256))
	}
	return out
}

func patternBytes(rng *rand.Rand, size int, tag byte) []byte {
	out := make([]byte, size)
	for index := range out {
		out[index] = tag ^ byte(rng.Intn(16))
	}
	return out
}

func sha256Hex(b []byte) string {
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])
}

func modeOf(info fs.FileInfo) uint32 {
	return uint32(info.Mode().Perm())
}

func statOf(info fs.FileInfo) *syscall.Stat_t {
	if stat, ok := info.Sys().(*syscall.Stat_t); ok {
		return stat
	}
	return nil
}

func mtimeNS(info fs.FileInfo) int64 {
	stat := statOf(info)
	if stat == nil {
		return 0
	}
	return stat.Mtim.Sec*1_000_000_000 + stat.Mtim.Nsec
}

func listUserXattrs(pathname string) (map[string][]byte, error) {
	size, err := syscall.Listxattr(pathname, nil)
	if err != nil {
		return nil, err
	}
	names := make([]byte, size)
	if size > 0 {
		if _, err := syscall.Listxattr(pathname, names); err != nil {
			return nil, err
		}
	}
	out := map[string][]byte{}
	for _, name := range strings.Split(string(names), "\x00") {
		if !strings.HasPrefix(name, "user.") {
			continue
		}
		value, err := getXattr(pathname, name)
		if err != nil {
			return nil, err
		}
		out[name] = value
	}
	return out, nil
}

func getXattr(pathname, name string) ([]byte, error) {
	size, err := syscall.Getxattr(pathname, name, nil)
	if err != nil {
		return nil, err
	}
	value := make([]byte, size)
	if size == 0 {
		return value, nil
	}
	n, err := syscall.Getxattr(pathname, name, value)
	if err != nil {
		return nil, err
	}
	return value[:n], nil
}

func verifyXattrs(pathname string, expected map[string][]byte) error {
	actual, err := listUserXattrs(pathname)
	if err != nil {
		return fmt.Errorf("listxattr: %w", err)
	}
	if len(actual) != len(expected) {
		return fmt.Errorf("xattr count %d != %d", len(actual), len(expected))
	}
	for name, want := range expected {
		got, ok := actual[name]
		if !ok {
			return fmt.Errorf("xattr %q missing", name)
		}
		if !bytes.Equal(got, want) {
			return fmt.Errorf("xattr %q value mismatch", name)
		}
	}
	return nil
}

func verifyFile(mount string, rel string, node *inode) error {
	pathname := absPath(mount, rel)
	info, err := os.Lstat(pathname)
	if err != nil {
		return fmt.Errorf("lstat: %w", err)
	}
	if !info.Mode().IsRegular() {
		return fmt.Errorf("expected regular file, mode %v", info.Mode())
	}
	if uint32(info.Size()) != uint32(len(node.content)) {
		return fmt.Errorf("size %d != %d", info.Size(), len(node.content))
	}
	if modeOf(info) != node.mode {
		return fmt.Errorf("mode %04o != %04o", modeOf(info), node.mode)
	}
	if stat := statOf(info); stat != nil && uint32(stat.Nlink) != node.nlink {
		return fmt.Errorf("nlink %d != %d", stat.Nlink, node.nlink)
	}
	got, err := os.ReadFile(pathname)
	if err != nil {
		return fmt.Errorf("read: %w", err)
	}
	if !bytes.Equal(got, node.content) {
		return fmt.Errorf("content mismatch: got %d bytes sha256=%s want %d bytes sha256=%s",
			len(got), sha256Hex(got), len(node.content), sha256Hex(node.content))
	}
	return verifyXattrs(pathname, node.xattrs)
}

func verifyDir(mount string, rel string, node *inode, m *model) error {
	pathname := absPath(mount, rel)
	info, err := os.Lstat(pathname)
	if err != nil {
		return fmt.Errorf("lstat: %w", err)
	}
	if !info.IsDir() {
		return fmt.Errorf("expected directory, mode %v", info.Mode())
	}
	if modeOf(info) != node.mode {
		return fmt.Errorf("mode %04o != %04o", modeOf(info), node.mode)
	}
	entries, err := os.ReadDir(pathname)
	if err != nil {
		return fmt.Errorf("readdir: %w", err)
	}
	var names []string
	for _, entry := range entries {
		if rel == "" && entry.Name() == weftDir {
			continue
		}
		names = append(names, entry.Name())
	}
	sort.Strings(names)
	expected := m.children(rel)
	if strings.Join(names, "\x00") != strings.Join(expected, "\x00") {
		return fmt.Errorf("directory entries %v != %v", names, expected)
	}
	return verifyXattrs(pathname, node.xattrs)
}

func verifySymlink(mount string, rel string, node *inode) error {
	pathname := absPath(mount, rel)
	info, err := os.Lstat(pathname)
	if err != nil {
		return fmt.Errorf("lstat: %w", err)
	}
	if info.Mode()&os.ModeSymlink == 0 {
		return fmt.Errorf("expected symlink, mode %v", info.Mode())
	}
	target, err := os.Readlink(pathname)
	if err != nil {
		return fmt.Errorf("readlink: %w", err)
	}
	if target != node.target {
		return fmt.Errorf("target %q != %q", target, node.target)
	}
	return nil
}

func verifyNode(mount string, rel string, node *inode, m *model) error {
	switch node.kind {
	case kindFile:
		return verifyFile(mount, rel, node)
	case kindDir:
		return verifyDir(mount, rel, node, m)
	case kindSymlink:
		return verifySymlink(mount, rel, node)
	}
	return fmt.Errorf("unknown node kind %q", node.kind)
}

func verifyAbsent(mount, rel string) error {
	_, err := os.Lstat(absPath(mount, rel))
	if err == nil {
		return fmt.Errorf("path %q still exists", rel)
	}
	if !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("lstat %q: %w", rel, err)
	}
	return nil
}

type manifestEntry struct {
	Path    string            `json:"path"`
	Type    string            `json:"type"`
	Mode    uint32            `json:"mode"`
	Size    int64             `json:"size,omitempty"`
	SHA256  string            `json:"sha256,omitempty"`
	Target  string            `json:"target,omitempty"`
	Xattrs  map[string]string `json:"xattrs,omitempty"`
	UID     uint32            `json:"uid"`
	GID     uint32            `json:"gid"`
	Nlink   uint32            `json:"nlink,omitempty"`
	MtimeNS int64             `json:"mtime_ns"`
}

type manifest struct {
	Schema  string          `json:"schema"`
	Entries []manifestEntry `json:"entries"`
	Digest  string          `json:"digest"`
}

func scanTree(mount string) ([]manifestEntry, error) {
	var entries []manifestEntry
	err := filepath.WalkDir(mount, func(pathname string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		rel, err := filepath.Rel(mount, pathname)
		if err != nil {
			return err
		}
		if rel == "." {
			return nil
		}
		rel = filepath.ToSlash(rel)
		if rel == weftDir || strings.HasPrefix(rel, weftDir+"/") {
			if entry.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}
		info, err := os.Lstat(pathname)
		if err != nil {
			return err
		}
		item := manifestEntry{
			Path: rel, Mode: modeOf(info), MtimeNS: mtimeNS(info),
		}
		if stat := statOf(info); stat != nil {
			item.UID = stat.Uid
			item.GID = stat.Gid
			item.Nlink = uint32(stat.Nlink)
		}
		switch {
		case info.Mode().IsRegular():
			item.Type = kindFile
			item.Size = info.Size()
			item.Nlink = item.Nlink
			data, err := os.ReadFile(pathname)
			if err != nil {
				return err
			}
			item.SHA256 = sha256Hex(data)
		case info.IsDir():
			item.Type = kindDir
			item.Nlink = 0
		case info.Mode()&os.ModeSymlink != 0:
			item.Type = kindSymlink
			target, err := os.Readlink(pathname)
			if err != nil {
				return err
			}
			item.Target = target
			item.Nlink = 0
		default:
			item.Type = "other"
		}
		if item.Type == kindFile || item.Type == kindDir {
			attrs, err := listUserXattrs(pathname)
			if err == nil && len(attrs) > 0 {
				item.Xattrs = map[string]string{}
				for name, value := range attrs {
					item.Xattrs[name] = base64.StdEncoding.EncodeToString(value)
				}
			}
		}
		entries = append(entries, item)
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i].Path < entries[j].Path })
	return entries, nil
}

func manifestFor(entries []manifestEntry) manifest {
	encoded, _ := json.Marshal(entries)
	return manifest{
		Schema: "weftfs.posix-manifest/1", Entries: entries, Digest: sha256Hex(encoded),
	}
}

func compareManifests(expected, actual []manifestEntry) []string {
	var problems []string
	want := map[string]manifestEntry{}
	got := map[string]manifestEntry{}
	for _, entry := range expected {
		want[entry.Path] = entry
	}
	for _, entry := range actual {
		got[entry.Path] = entry
	}
	paths := map[string]bool{}
	for rel := range want {
		paths[rel] = true
	}
	for rel := range got {
		paths[rel] = true
	}
	var ordered []string
	for rel := range paths {
		ordered = append(ordered, rel)
	}
	sort.Strings(ordered)
	for _, rel := range ordered {
		w, wOK := want[rel]
		g, gOK := got[rel]
		if !wOK {
			problems = append(problems, fmt.Sprintf("%s: unexpected entry", rel))
			continue
		}
		if !gOK {
			problems = append(problems, fmt.Sprintf("%s: missing entry", rel))
			continue
		}
		if w.Type != g.Type {
			problems = append(problems, fmt.Sprintf("%s: type %q != %q", rel, g.Type, w.Type))
			continue
		}
		if w.Mode != g.Mode {
			problems = append(problems, fmt.Sprintf("%s: mode %04o != %04o", rel, g.Mode, w.Mode))
		}
		if w.Size != g.Size {
			problems = append(problems, fmt.Sprintf("%s: size %d != %d", rel, g.Size, w.Size))
		}
		if w.SHA256 != g.SHA256 {
			problems = append(problems, fmt.Sprintf("%s: sha256 %s != %s", rel, g.SHA256, w.SHA256))
		}
		if w.Target != g.Target {
			problems = append(problems, fmt.Sprintf("%s: target %q != %q", rel, g.Target, w.Target))
		}
		if w.UID != g.UID || w.GID != g.GID {
			problems = append(problems, fmt.Sprintf("%s: owner %d:%d != %d:%d", rel, g.UID, g.GID, w.UID, w.GID))
		}
		if w.Nlink != g.Nlink {
			problems = append(problems, fmt.Sprintf("%s: nlink %d != %d", rel, g.Nlink, w.Nlink))
		}
		if w.MtimeNS != g.MtimeNS {
			problems = append(problems, fmt.Sprintf("%s: mtime %d != %d", rel, g.MtimeNS, w.MtimeNS))
		}
		if len(w.Xattrs) != len(g.Xattrs) {
			problems = append(problems, fmt.Sprintf("%s: xattr count %d != %d", rel, len(g.Xattrs), len(w.Xattrs)))
		} else {
			for name, value := range w.Xattrs {
				if g.Xattrs[name] != value {
					problems = append(problems, fmt.Sprintf("%s: xattr %q mismatch", rel, name))
				}
			}
		}
		if len(problems) >= 40 {
			return problems
		}
	}
	return problems
}

type failure struct {
	OpIndex  int    `json:"op_index"`
	Op       string `json:"op"`
	Path     string `json:"path,omitempty"`
	Detail   string `json:"detail"`
	Expected string `json:"expected,omitempty"`
	Actual   string `json:"actual,omitempty"`
}

type runResult struct {
	Schema        string         `json:"schema"`
	Impl          string         `json:"impl"`
	Seed          int64          `json:"seed"`
	OpsRequested  int            `json:"ops_requested"`
	OpsCompleted  int            `json:"ops_completed"`
	OpsSkipped    int            `json:"ops_skipped"`
	OpCounts      map[string]int `json:"op_counts"`
	Failures      []failure      `json:"failures"`
	ManifestPath  string         `json:"manifest_path,omitempty"`
	ManifestSHA   string         `json:"manifest_sha256,omitempty"`
	ManifestCount int            `json:"manifest_entries,omitempty"`
	VerifyOnly    bool           `json:"verify_only"`
	DurationMS    int64          `json:"duration_ms"`
	OK            bool           `json:"ok"`
}

var opNames = []string{
	"create", "write", "read", "truncate", "append", "rename", "rename_over",
	"link", "unlink", "mkdir", "rmdir", "symlink", "readlink", "xattr",
	"fsync", "open_unlinked",
}

type torture struct {
	mount   string
	m       *model
	rng     *rand.Rand
	counter int
}

func (t *torture) nextPath(dir, prefix string, limit int) (string, bool) {
	name, ok := uniqueName(t.m, dir, prefix, limit)
	if !ok {
		return "", false
	}
	return join(dir, name), true
}

func (t *torture) pickDir() string {
	dirs := t.m.dirs()
	return dirs[t.rng.Intn(len(dirs))]
}

func (t *torture) pickFile() (string, *inode, bool) {
	files := t.m.files()
	if len(files) == 0 {
		return "", nil, false
	}
	rel := files[t.rng.Intn(len(files))]
	return rel, t.m.entries[rel], true
}

func (t *torture) apply(op string) (string, error) {
	switch op {
	case "create":
		if len(t.m.entries) > 96 {
			return "", errSkip
		}
		dir := t.pickDir()
		rel, ok := t.nextPath(dir, "f", 16)
		if !ok {
			return "", errSkip
		}
		mode := []uint32{0o600, 0o644, 0o640}[t.rng.Intn(3)]
		content := randomBytes(t.rng, 1+t.rng.Intn(96))
		file, err := os.OpenFile(absPath(t.mount, rel), os.O_CREATE|os.O_EXCL|os.O_RDWR, os.FileMode(mode))
		if err != nil {
			return rel, fmt.Errorf("create: %w", err)
		}
		if _, err := file.Write(content); err != nil {
			file.Close()
			return rel, fmt.Errorf("create write: %w", err)
		}
		if err := file.Close(); err != nil {
			return rel, fmt.Errorf("create close: %w", err)
		}
		node := &inode{kind: kindFile, content: content, mode: mode, nlink: 1, xattrs: map[string][]byte{}}
		t.m.add(rel, node)
		return rel, verifyFile(t.mount, rel, node)

	case "write":
		rel, node, ok := t.pickFile()
		if !ok {
			return "", errSkip
		}
		offset := t.rng.Intn(len(node.content) + 17)
		data := patternBytes(t.rng, 1+t.rng.Intn(64), byte('a'+t.rng.Intn(26)))
		file, err := os.OpenFile(absPath(t.mount, rel), os.O_RDWR, 0)
		if err != nil {
			return rel, fmt.Errorf("open: %w", err)
		}
		if _, err := file.WriteAt(data, int64(offset)); err != nil {
			file.Close()
			return rel, fmt.Errorf("writeat: %w", err)
		}
		if err := file.Close(); err != nil {
			return rel, fmt.Errorf("close: %w", err)
		}
		if offset > len(node.content) {
			node.content = append(node.content, make([]byte, offset-len(node.content))...)
		}
		end := offset + len(data)
		if end > len(node.content) {
			node.content = append(node.content, make([]byte, end-len(node.content))...)
		}
		copy(node.content[offset:end], data)
		return rel, verifyFile(t.mount, rel, node)

	case "read":
		rel, node, ok := t.pickFile()
		if !ok {
			return "", errSkip
		}
		data, err := os.ReadFile(absPath(t.mount, rel))
		if err != nil {
			return rel, fmt.Errorf("read: %w", err)
		}
		if !bytes.Equal(data, node.content) {
			return rel, fmt.Errorf("read mismatch: got %s want %s", sha256Hex(data), sha256Hex(node.content))
		}
		return rel, verifyFile(t.mount, rel, node)

	case "truncate":
		rel, node, ok := t.pickFile()
		if !ok {
			return "", errSkip
		}
		size := t.rng.Intn(len(node.content) + 33)
		if err := os.Truncate(absPath(t.mount, rel), int64(size)); err != nil {
			return rel, fmt.Errorf("truncate: %w", err)
		}
		if size <= len(node.content) {
			node.content = node.content[:size]
		} else {
			node.content = append(node.content, make([]byte, size-len(node.content))...)
		}
		return rel, verifyFile(t.mount, rel, node)

	case "append":
		rel, node, ok := t.pickFile()
		if !ok {
			return "", errSkip
		}
		data := randomBytes(t.rng, 1+t.rng.Intn(32))
		file, err := os.OpenFile(absPath(t.mount, rel), os.O_WRONLY|os.O_APPEND, 0)
		if err != nil {
			return rel, fmt.Errorf("open append: %w", err)
		}
		if _, err := file.Write(data); err != nil {
			file.Close()
			return rel, fmt.Errorf("append: %w", err)
		}
		if err := file.Close(); err != nil {
			return rel, fmt.Errorf("close: %w", err)
		}
		node.content = append(node.content, data...)
		return rel, verifyFile(t.mount, rel, node)

	case "rename":
		rel, node, ok := t.pickFile()
		if !ok {
			return "", errSkip
		}
		dir := t.pickDir()
		dst, ok := t.nextPath(dir, "r", 32)
		if !ok || dst == rel {
			return "", errSkip
		}
		if err := os.Rename(absPath(t.mount, rel), absPath(t.mount, dst)); err != nil {
			return rel, fmt.Errorf("rename: %w", err)
		}
		delete(t.m.entries, rel)
		t.m.add(dst, node)
		if err := verifyAbsent(t.mount, rel); err != nil {
			return rel, err
		}
		return dst, verifyNode(t.mount, dst, node, t.m)

	case "rename_over":
		rel, node, ok := t.pickFile()
		if !ok {
			return "", errSkip
		}
		targets := t.m.files()
		var dst string
		for _, candidate := range targets {
			if candidate != rel && t.m.entries[candidate] != node {
				dst = candidate
				break
			}
		}
		if dst == "" {
			return "", errSkip
		}
		if err := os.Rename(absPath(t.mount, rel), absPath(t.mount, dst)); err != nil {
			return rel, fmt.Errorf("rename over: %w", err)
		}
		delete(t.m.entries, rel)
		replaced := t.m.entries[dst]
		if replaced != nil && replaced != node && replaced.nlink > 0 {
			replaced.nlink--
		}
		t.m.entries[dst] = node
		return dst, verifyNode(t.mount, dst, node, t.m)

	case "link":
		rel, node, ok := t.pickFile()
		if !ok || len(t.m.entries) > 96 {
			return "", errSkip
		}
		dir := t.pickDir()
		dst, ok := t.nextPath(dir, "h", 12)
		if !ok || dst == rel {
			return "", errSkip
		}
		if err := os.Link(absPath(t.mount, rel), absPath(t.mount, dst)); err != nil {
			return rel, fmt.Errorf("link: %w", err)
		}
		node.nlink++
		t.m.add(dst, node)
		if err := verifyFile(t.mount, rel, node); err != nil {
			return rel, err
		}
		return dst, verifyFile(t.mount, dst, node)

	case "unlink":
		rel, node, ok := t.pickFile()
		if !ok {
			return "", errSkip
		}
		if err := os.Remove(absPath(t.mount, rel)); err != nil {
			return rel, fmt.Errorf("unlink: %w", err)
		}
		delete(t.m.entries, rel)
		if node.nlink > 0 {
			node.nlink--
		}
		return rel, verifyAbsent(t.mount, rel)

	case "mkdir":
		if len(t.m.entries) > 96 {
			return "", errSkip
		}
		rel, ok := t.nextPath("", "m", 8)
		if !ok {
			return "", errSkip
		}
		mode := []uint32{0o700, 0o755}[t.rng.Intn(2)]
		if err := os.Mkdir(absPath(t.mount, rel), os.FileMode(mode)); err != nil {
			return rel, fmt.Errorf("mkdir: %w", err)
		}
		node := &inode{kind: kindDir, mode: mode, nlink: 2, xattrs: map[string][]byte{}}
		t.m.add(rel, node)
		return rel, verifyDir(t.mount, rel, node, t.m)

	case "rmdir":
		dirs := t.m.emptyDirs()
		if len(dirs) == 0 {
			return "", errSkip
		}
		rel := dirs[t.rng.Intn(len(dirs))]
		if err := os.Remove(absPath(t.mount, rel)); err != nil {
			return rel, fmt.Errorf("rmdir: %w", err)
		}
		delete(t.m.entries, rel)
		return rel, verifyAbsent(t.mount, rel)

	case "symlink":
		rel, node, ok := t.pickFile()
		if !ok || len(t.m.entries) > 96 {
			return "", errSkip
		}
		dir := t.pickDir()
		linkRel, ok := t.nextPath(dir, "s", 8)
		if !ok {
			return "", errSkip
		}
		parentAbs := absPath(t.mount, dir)
		target, err := filepath.Rel(parentAbs, absPath(t.mount, rel))
		if err != nil {
			return "", errSkip
		}
		if err := os.Symlink(target, absPath(t.mount, linkRel)); err != nil {
			return linkRel, fmt.Errorf("symlink: %w", err)
		}
		_ = node
		link := &inode{kind: kindSymlink, target: target, nlink: 1, xattrs: map[string][]byte{}}
		t.m.add(linkRel, link)
		return linkRel, verifySymlink(t.mount, linkRel, link)

	case "readlink":
		links := t.m.symlinks()
		if len(links) == 0 {
			return "", errSkip
		}
		rel := links[t.rng.Intn(len(links))]
		return rel, verifySymlink(t.mount, rel, t.m.entries[rel])

	case "xattr":
		var rel string
		var node *inode
		files := t.m.files()
		dirs := t.m.dirs()
		if len(files) > 0 && (len(dirs) == 0 || t.rng.Intn(2) == 0) {
			rel = files[t.rng.Intn(len(files))]
		} else if len(dirs) > 0 {
			rel = dirs[t.rng.Intn(len(dirs))]
		}
		if rel == "" {
			return "", errSkip
		}
		node = t.m.entries[rel]
		if node == nil || node.kind == kindSymlink {
			return "", errSkip
		}
		if node.xattrs == nil {
			node.xattrs = map[string][]byte{}
		}
		name := fmt.Sprintf("user.weft%d", t.rng.Intn(4))
		pathname := absPath(t.mount, rel)
		switch t.rng.Intn(4) {
		case 0, 1:
			value := randomBytes(t.rng, 1+t.rng.Intn(16))
			if err := syscall.Setxattr(pathname, name, value, 0); err != nil {
				return rel, fmt.Errorf("setxattr: %w", err)
			}
			node.xattrs[name] = value
		case 2:
			got, err := getXattr(pathname, name)
			want, ok := node.xattrs[name]
			if !ok {
				if !errors.Is(err, syscall.ENODATA) {
					return rel, fmt.Errorf("getxattr missing: got err %v", err)
				}
			} else {
				if err != nil {
					return rel, fmt.Errorf("getxattr: %w", err)
				}
				if !bytes.Equal(got, want) {
					return rel, fmt.Errorf("xattr value mismatch")
				}
			}
		case 3:
			if _, ok := node.xattrs[name]; ok {
				if err := syscall.Removexattr(pathname, name); err != nil {
					return rel, fmt.Errorf("removexattr: %w", err)
				}
				delete(node.xattrs, name)
			} else if err := syscall.Removexattr(pathname, name); !errors.Is(err, syscall.ENODATA) {
				return rel, fmt.Errorf("removexattr missing: got err %v", err)
			}
		}
		return rel, verifyXattrs(pathname, node.xattrs)

	case "fsync":
		rel, node, ok := t.pickFile()
		if !ok {
			return "", errSkip
		}
		file, err := os.OpenFile(absPath(t.mount, rel), os.O_RDWR, 0)
		if err != nil {
			return rel, fmt.Errorf("open: %w", err)
		}
		if err := file.Sync(); err != nil {
			file.Close()
			return rel, fmt.Errorf("fsync: %w", err)
		}
		if err := file.Close(); err != nil {
			return rel, fmt.Errorf("close: %w", err)
		}
		return rel, verifyFile(t.mount, rel, node)

	case "open_unlinked":
		rel, ok := t.nextPath("", "u", 8)
		if !ok {
			return "", errSkip
		}
		content := randomBytes(t.rng, 1+t.rng.Intn(64))
		pathname := absPath(t.mount, rel)
		// Write and close first, then open a read-only descriptor and unlink it.
		// This is the POSIX open-unlinked read contract both frontends document.
		writer, err := os.OpenFile(pathname, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
		if err != nil {
			return rel, fmt.Errorf("open-unlinked create: %w", err)
		}
		if _, err := writer.Write(content); err != nil {
			writer.Close()
			return rel, fmt.Errorf("open-unlinked write: %w", err)
		}
		if err := writer.Close(); err != nil {
			return rel, fmt.Errorf("open-unlinked close writer: %w", err)
		}
		file, err := os.Open(pathname)
		if err != nil {
			return rel, fmt.Errorf("open-unlinked open read: %w", err)
		}
		if err := os.Remove(pathname); err != nil {
			file.Close()
			return rel, fmt.Errorf("open-unlinked unlink: %w", err)
		}
		if err := verifyAbsent(t.mount, rel); err != nil {
			file.Close()
			return rel, err
		}
		got, err := io.ReadAll(file)
		if err != nil {
			file.Close()
			return rel, fmt.Errorf("open-unlinked read: %w", err)
		}
		if err := file.Close(); err != nil {
			return rel, fmt.Errorf("open-unlinked close: %w", err)
		}
		if !bytes.Equal(got, content) {
			return rel, fmt.Errorf("open-unlinked content mismatch")
		}
		return rel, nil
	}
	return "", fmt.Errorf("unknown operation %q", op)
}

func fullVerify(mount string, m *model) error {
	var problems []string
	// Verify the root directory's children explicitly; it has no model inode
	// because its mode is created by the frontend.
	entries, err := os.ReadDir(mount)
	if err != nil {
		return fmt.Errorf("readdir root: %w", err)
	}
	var names []string
	for _, entry := range entries {
		if entry.Name() == weftDir {
			continue
		}
		names = append(names, entry.Name())
	}
	sort.Strings(names)
	expected := m.children("")
	if strings.Join(names, "\x00") != strings.Join(expected, "\x00") {
		problems = append(problems, fmt.Sprintf("/: directory entries %v != %v", names, expected))
	}
	for rel, node := range m.entries {
		if err := verifyNode(mount, rel, node, m); err != nil {
			problems = append(problems, fmt.Sprintf("%s: %v", rel, err))
			if len(problems) >= 20 {
				break
			}
		}
	}
	sort.Strings(problems)
	if len(problems) > 0 {
		return fmt.Errorf("full verification: %s", strings.Join(problems, "; "))
	}
	return nil
}

func runTorture(mount, impl string, seed int64, ops int, manifestPath string) runResult {
	start := time.Now()
	rng := rand.New(rand.NewSource(seed))
	m := newModel()
	t := &torture{mount: mount, m: m, rng: rng}
	result := runResult{
		Schema: "weftfs.posix-torture.run/1", Impl: impl, Seed: seed,
		OpsRequested: ops, OpCounts: map[string]int{}, Failures: []failure{},
		ManifestPath: manifestPath,
	}
	for _, dir := range m.dirs() {
		if dir == "" {
			continue
		}
		pathname := absPath(mount, dir)
		if err := os.Mkdir(pathname, 0o755); err != nil && !errors.Is(err, os.ErrExist) {
			result.Failures = append(result.Failures, failure{OpIndex: -1, Op: "setup", Path: dir, Detail: err.Error()})
			result.OK = false
			result.DurationMS = time.Since(start).Milliseconds()
			return result
		}
	}
	if err := fullVerify(mount, m); err != nil {
		result.Failures = append(result.Failures, failure{OpIndex: -1, Op: "setup", Detail: err.Error()})
		result.OK = false
		result.DurationMS = time.Since(start).Milliseconds()
		return result
	}

	deck := make([]string, 0, len(opNames)*4)
	for len(deck) < ops {
		shuffled := append([]string(nil), opNames...)
		rng.Shuffle(len(shuffled), func(i, j int) { shuffled[i], shuffled[j] = shuffled[j], shuffled[i] })
		deck = append(deck, shuffled...)
	}
	for index := 0; index < ops; index++ {
		op := deck[index]
		result.OpCounts[op]++
		rel, err := t.apply(op)
		if errors.Is(err, errSkip) {
			result.OpsSkipped++
			continue
		}
		result.OpsCompleted++
		if err != nil {
			result.Failures = append(result.Failures, failure{
				OpIndex: index, Op: op, Path: rel, Detail: err.Error(),
			})
			if len(result.Failures) >= 20 {
				break
			}
		}
		if (index+1)%100 == 0 {
			if err := fullVerify(mount, m); err != nil {
				result.Failures = append(result.Failures, failure{
					OpIndex: index, Op: "full_verify", Detail: err.Error(),
				})
				if len(result.Failures) >= 20 {
					break
				}
			}
		}
	}
	if len(result.Failures) == 0 {
		if err := fullVerify(mount, m); err != nil {
			result.Failures = append(result.Failures, failure{OpIndex: ops, Op: "final_verify", Detail: err.Error()})
		}
	}
	entries, err := scanTree(mount)
	if err != nil {
		result.Failures = append(result.Failures, failure{OpIndex: ops, Op: "manifest", Detail: err.Error()})
	} else {
		doc := manifestFor(entries)
		encoded, _ := json.MarshalIndent(doc, "", "  ")
		if err := os.WriteFile(manifestPath, append(encoded, '\n'), 0o644); err != nil {
			result.Failures = append(result.Failures, failure{OpIndex: ops, Op: "manifest_write", Detail: err.Error()})
		}
		result.ManifestSHA = doc.Digest
		result.ManifestCount = len(entries)
	}
	result.OK = len(result.Failures) == 0
	result.DurationMS = time.Since(start).Milliseconds()
	return result
}

func runVerifyOnly(mount, impl, manifestPath string) runResult {
	start := time.Now()
	result := runResult{
		Schema: "weftfs.posix-torture.run/1", Impl: impl, VerifyOnly: true,
		OpCounts: map[string]int{}, Failures: []failure{}, ManifestPath: manifestPath,
	}
	raw, err := os.ReadFile(manifestPath)
	if err != nil {
		result.Failures = append(result.Failures, failure{Op: "manifest_read", Detail: err.Error()})
		result.OK = false
		result.DurationMS = time.Since(start).Milliseconds()
		return result
	}
	var expected manifest
	if err := json.Unmarshal(raw, &expected); err != nil {
		result.Failures = append(result.Failures, failure{Op: "manifest_parse", Detail: err.Error()})
		result.OK = false
		result.DurationMS = time.Since(start).Milliseconds()
		return result
	}
	actual, err := scanTree(mount)
	if err != nil {
		result.Failures = append(result.Failures, failure{Op: "scan", Detail: err.Error()})
		result.OK = false
		result.DurationMS = time.Since(start).Milliseconds()
		return result
	}
	for _, problem := range compareManifests(expected.Entries, actual) {
		result.Failures = append(result.Failures, failure{Op: "verify_only", Detail: problem})
	}
	result.ManifestCount = len(actual)
	actualDoc := manifestFor(actual)
	result.ManifestSHA = actualDoc.Digest
	if len(result.Failures) == 0 && actualDoc.Digest != expected.Digest {
		result.Failures = append(result.Failures, failure{
			Op: "verify_only", Detail: fmt.Sprintf("manifest digest %s != %s", actualDoc.Digest, expected.Digest),
		})
	}
	result.OK = len(result.Failures) == 0
	result.DurationMS = time.Since(start).Milliseconds()
	return result
}

func main() {
	mount := flag.String("mount", "", "mountpoint to exercise")
	seed := flag.Int64("seed", 1, "deterministic PRNG seed")
	ops := flag.Int("ops", 2000, "randomized operation count")
	impl := flag.String("impl", "unknown", "implementation label")
	manifestPath := flag.String("manifest", "", "manifest path (written, or read with --verify-only)")
	verifyOnly := flag.Bool("verify-only", false, "check the mount against --manifest instead of running operations")
	resultPath := flag.String("result", "", "write the run result JSON here (default stdout)")
	flag.Parse()
	if *mount == "" || *manifestPath == "" {
		fmt.Fprintln(os.Stderr, "usage: posix_torture --mount DIR --manifest FILE [--seed N] [--ops N] [--impl NAME] [--verify-only] [--result FILE]")
		os.Exit(64)
	}
	var result runResult
	if *verifyOnly {
		result = runVerifyOnly(*mount, *impl, *manifestPath)
	} else {
		if *ops < 1 {
			fmt.Fprintln(os.Stderr, "posix_torture: --ops must be positive")
			os.Exit(64)
		}
		result = runTorture(*mount, *impl, *seed, *ops, *manifestPath)
	}
	encoded, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		fmt.Fprintf(os.Stderr, "posix_torture: marshal: %v\n", err)
		os.Exit(65)
	}
	if *resultPath != "" {
		if err := os.WriteFile(*resultPath, append(encoded, '\n'), 0o644); err != nil {
			fmt.Fprintf(os.Stderr, "posix_torture: write result: %v\n", err)
			os.Exit(65)
		}
	} else {
		fmt.Println(string(encoded))
	}
	if !result.OK {
		for _, item := range result.Failures {
			fmt.Fprintf(os.Stderr, "FAIL op=%d %s %s: %s\n", item.OpIndex, item.Op, item.Path, item.Detail)
		}
		os.Exit(1)
	}
}
