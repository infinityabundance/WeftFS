// Command bundle_manifest assembles the receipt-bundle manifest from a TSV
// stream written by scripts/package-receipts.sh.
//
// It is stdlib-only and deterministic: every array is sorted before emission,
// and paths are relative to the bundle root.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
)

type court struct {
	Name             string `json:"name"`
	Image            string `json:"image"`
	ImageID          string `json:"image_id"`
	Dockerfile       string `json:"dockerfile"`
	DockerfileSHA256 string `json:"dockerfile_sha256"`
}

type repository struct {
	Name         string `json:"name"`
	Path         string `json:"path"`
	Branch       string `json:"branch"`
	Revision     string `json:"revision"`
	Worktree     string `json:"worktree"`
	Dirty        bool   `json:"dirty"`
	ChangedFiles int    `json:"changed_files"`
}

type fileRecord struct {
	Path         string `json:"path"`
	Category     string `json:"category"`
	SHA256       string `json:"sha256"`
	Size         int64  `json:"size"`
	SourceSHA256 string `json:"source_sha256"`
	Sanitized    bool   `json:"sanitized"`
}

type corpusItem struct {
	Name   string `json:"name"`
	Path   string `json:"path"`
	SHA256 string `json:"sha256"`
	Size   int64  `json:"size"`
}

type receiptItem struct {
	Path   string `json:"path"`
	SHA256 string `json:"sha256"`
	Size   int64  `json:"size"`
}

type manifest struct {
	Schema       string            `json:"schema"`
	GeneratedAt  string            `json:"generated_at"`
	Stamp        string            `json:"stamp"`
	Bundle       map[string]any    `json:"bundle"`
	Host         map[string]string `json:"host"`
	Docker       map[string]string `json:"docker"`
	CourtLimits  map[string]string `json:"court_limits"`
	CourtImages  []court           `json:"court_images"`
	Repositories []repository      `json:"repositories"`
	Revisions    map[string]string `json:"revisions"`
	Dmesg        map[string]string `json:"dmesg"`
	Files        []fileRecord      `json:"files"`
	Corpus       []corpusItem      `json:"corpus"`
	Receipts     []receiptItem     `json:"receipts"`
	Logs         []string          `json:"raw_logs"`
	Notes        []string          `json:"notes"`
}

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "bundle_manifest: "+format+"\n", args...)
	os.Exit(65)
}

func main() {
	out := manifest{
		Schema:      "weftfs.receipt-bundle/1",
		Host:        map[string]string{},
		Docker:      map[string]string{},
		CourtLimits: map[string]string{},
		Revisions:   map[string]string{},
		Dmesg:       map[string]string{},
	}
	meta := map[string]string{}
	scanner := bufio.NewScanner(os.Stdin)
	scanner.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	for scanner.Scan() {
		line := scanner.Text()
		if line == "" {
			continue
		}
		fields := strings.Split(line, "\t")
		switch fields[0] {
		case "META":
			if len(fields) != 3 {
				fail("META needs 3 fields")
			}
			meta[fields[1]] = fields[2]
		case "NOTE":
			if len(fields) != 2 {
				fail("NOTE needs 2 fields")
			}
			out.Notes = append(out.Notes, fields[1])
		case "COURT":
			if len(fields) != 6 {
				fail("COURT needs 6 fields")
			}
			out.CourtImages = append(out.CourtImages, court{
				Name: fields[1], Image: fields[2], ImageID: fields[3],
				Dockerfile: fields[4], DockerfileSHA256: fields[5],
			})
		case "REPO":
			if len(fields) != 7 {
				fail("REPO needs 7 fields")
			}
			changed, err := strconv.Atoi(fields[6])
			if err != nil {
				fail("bad REPO changed count")
			}
			out.Repositories = append(out.Repositories, repository{
				Name: fields[1], Path: fields[2], Branch: fields[3], Revision: fields[4],
				Worktree: fields[5], Dirty: changed != 0, ChangedFiles: changed,
			})
			out.Revisions[fields[1]] = fields[4]
		case "FILE":
			if len(fields) != 7 {
				fail("FILE needs 7 fields")
			}
			size, err := strconv.ParseInt(fields[4], 10, 64)
			if err != nil {
				fail("bad FILE size")
			}
			out.Files = append(out.Files, fileRecord{
				Category: fields[1], Path: fields[2], SHA256: fields[3], Size: size,
				SourceSHA256: fields[5], Sanitized: fields[6] == "true",
			})
		case "CORPUS":
			if len(fields) != 5 {
				fail("CORPUS needs 5 fields")
			}
			size, err := strconv.ParseInt(fields[4], 10, 64)
			if err != nil {
				fail("bad CORPUS size")
			}
			out.Corpus = append(out.Corpus, corpusItem{
				Name: fields[1], Path: fields[2], SHA256: fields[3], Size: size,
			})
		case "RECEIPT":
			if len(fields) != 4 {
				fail("RECEIPT needs 4 fields")
			}
			size, err := strconv.ParseInt(fields[3], 10, 64)
			if err != nil {
				fail("bad RECEIPT size")
			}
			out.Receipts = append(out.Receipts, receiptItem{
				Path: fields[1], SHA256: fields[2], Size: size,
			})
		case "LOG":
			if len(fields) != 2 {
				fail("LOG needs 2 fields")
			}
			out.Logs = append(out.Logs, fields[1])
		default:
			fail("unknown record type %q", fields[0])
		}
	}
	if err := scanner.Err(); err != nil {
		fail("%v", err)
	}

	out.GeneratedAt = meta["generated_at"]
	out.Stamp = meta["stamp"]
	out.Bundle = map[string]any{
		"format":       "tar.zst",
		"verifier":     "scripts/verify-receipts.sh",
		"checksums":    "CHECKSUMS.sha256",
		"manifest":     "manifest.json",
		"revisions":    "REVISIONS.txt",
		"dmesg_status": meta["dmesg_status"],
	}
	out.Host = map[string]string{
		"kernel":       meta["kernel"],
		"uname":        meta["uname"],
		"arch":         meta["arch"],
		"distribution": meta["distribution"],
	}
	out.Docker = map[string]string{
		"client": meta["docker_client"],
		"server": meta["docker_server"],
	}
	out.CourtLimits = map[string]string{
		"memory":      meta["memory"],
		"memory_swap": meta["memory_swap"],
		"cpus":        meta["cpus"],
		"pids":        meta["pids"],
		"tmpfs_tmp":   meta["tmpfs_tmp"],
	}
	out.Dmesg = map[string]string{
		"status": meta["dmesg_status"],
		"path":   meta["dmesg_path"],
	}
	if out.Dmesg["path"] == "" {
		out.Dmesg["path"] = "dmesg.txt"
	}
	if out.Dmesg["status"] == "" {
		out.Dmesg["status"] = "unavailable (no CAP_SYSLOG)"
	}

	sort.Slice(out.CourtImages, func(i, j int) bool { return out.CourtImages[i].Name < out.CourtImages[j].Name })
	sort.Slice(out.Repositories, func(i, j int) bool { return out.Repositories[i].Name < out.Repositories[j].Name })
	sort.Slice(out.Files, func(i, j int) bool { return out.Files[i].Path < out.Files[j].Path })
	sort.Slice(out.Corpus, func(i, j int) bool { return out.Corpus[i].Path < out.Corpus[j].Path })
	sort.Slice(out.Receipts, func(i, j int) bool { return out.Receipts[i].Path < out.Receipts[j].Path })
	sort.Strings(out.Logs)

	encoded, err := json.MarshalIndent(out, "", "  ")
	if err != nil {
		fail("marshal: %v", err)
	}
	fmt.Println(string(encoded))
}
