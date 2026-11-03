//! Assemble the machine-readable showcase receipt from the script's TSV log.
//!
//! The bash driver writes only tab-separated records whose payload fields are
//! base64-encoded, so paths, quotes, newlines, and command text cannot corrupt
//! the receipt. This program is stdlib-only and deterministic: map keys are
//! emitted in sorted order by encoding/json.

package main

import (
	"bufio"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"
)

const schema = "weftfs.showcase.receipt/1"

type Run struct {
	StartedAt  string `json:"started_at"`
	FinishedAt string `json:"finished_at"`
	DurationMS int64  `json:"duration_ms"`
}

type Repository struct {
	Name     string `json:"name"`
	Revision string `json:"revision"`
}

type Fixture struct {
	Name   string `json:"name"`
	Path   string `json:"path"`
	SHA256 string `json:"sha256"`
	Bytes  int64  `json:"bytes"`
}

type Assertion struct {
	Name     string `json:"name"`
	Expected string `json:"expected"`
	Actual   string `json:"actual"`
	Pass     bool   `json:"pass"`
}

type Output struct {
	Label   string `json:"label"`
	Content string `json:"content"`
}

type Artifact struct {
	Kind   string `json:"kind"`
	Path   string `json:"path"`
	SHA256 string `json:"sha256"`
}

type Step struct {
	ID         string      `json:"id"`
	Title      string      `json:"title"`
	Status     string      `json:"status"`
	Notes      string      `json:"notes,omitempty"`
	Commands   []string    `json:"commands"`
	Assertions []Assertion `json:"assertions"`
	Outputs    []Output    `json:"outputs"`
	Artifacts  []Artifact  `json:"artifacts"`
}

type Summary struct {
	Total   int  `json:"total"`
	Pass    int  `json:"pass"`
	Fail    int  `json:"fail"`
	Blocked int  `json:"blocked"`
	AllPass bool `json:"all_pass"`
}

type Receipt struct {
	Schema       string            `json:"schema"`
	GeneratedAt  string            `json:"generated_at"`
	Run          Run               `json:"run"`
	Environment  map[string]string `json:"environment"`
	Repositories []Repository      `json:"repositories"`
	Fixtures     []Fixture         `json:"fixtures"`
	Steps        []Step            `json:"steps"`
	Notes        []string          `json:"notes,omitempty"`
	Summary      Summary           `json:"summary"`
}

func decode(field string) string {
	bytes, err := base64.StdEncoding.DecodeString(field)
	if err != nil {
		fmt.Fprintf(os.Stderr, "receiptgen: bad base64 field: %v\n", err)
		os.Exit(65)
	}
	return string(bytes)
}

func main() {
	receipt := Receipt{
		Schema:      schema,
		Environment: map[string]string{},
		Run:         Run{},
	}
	findStep := func(id string) *Step {
		for index := range receipt.Steps {
			if receipt.Steps[index].ID == id {
				return &receipt.Steps[index]
			}
		}
		return nil
	}

	scanner := bufio.NewScanner(os.Stdin)
	scanner.Buffer(make([]byte, 0, 1024*1024), 64*1024*1024)
	for scanner.Scan() {
		line := scanner.Text()
		if line == "" {
			continue
		}
		fields := strings.Split(line, "\t")
		switch fields[0] {
		case "M":
			if len(fields) != 3 {
				fail("M record needs 3 fields")
			}
			key, value := fields[1], decode(fields[2])
			switch {
			case key == "generated_at":
				receipt.GeneratedAt = value
			case key == "run.started_at":
				receipt.Run.StartedAt = value
			case key == "run.finished_at":
				receipt.Run.FinishedAt = value
			case key == "run.duration_ms":
				number, err := strconv.ParseInt(value, 10, 64)
				if err != nil {
					fail("bad run.duration_ms")
				}
				receipt.Run.DurationMS = number
			case strings.HasPrefix(key, "env."):
				receipt.Environment[strings.TrimPrefix(key, "env.")] = value
			}
		case "R":
			if len(fields) != 3 {
				fail("R record needs 3 fields")
			}
			receipt.Repositories = append(receipt.Repositories, Repository{
				Name: fields[1], Revision: decode(fields[2]),
			})
		case "F":
			if len(fields) != 5 {
				fail("F record needs 5 fields")
			}
			number, err := strconv.ParseInt(decode(fields[4]), 10, 64)
			if err != nil {
				fail("bad fixture byte count")
			}
			receipt.Fixtures = append(receipt.Fixtures, Fixture{
				Name: fields[1], Path: decode(fields[2]), SHA256: decode(fields[3]), Bytes: number,
			})
		case "S":
			if len(fields) != 4 {
				fail("S record needs 4 fields")
			}
			step := Step{
				ID: fields[1], Title: decode(fields[2]), Notes: decode(fields[3]),
				Status: "blocked", Commands: []string{}, Assertions: []Assertion{},
				Outputs: []Output{}, Artifacts: []Artifact{},
			}
			receipt.Steps = append(receipt.Steps, step)
		case "C":
			if len(fields) != 3 {
				fail("C record needs 3 fields")
			}
			target := findStep(fields[1])
			if target == nil {
				fail("command for unknown step")
			}
			target.Commands = append(target.Commands, decode(fields[2]))
		case "A":
			if len(fields) != 6 {
				fail("A record needs 6 fields")
			}
			target := findStep(fields[1])
			if target == nil {
				fail("assertion for unknown step")
			}
			target.Assertions = append(target.Assertions, Assertion{
				Name: decode(fields[2]), Expected: decode(fields[3]),
				Actual: decode(fields[4]), Pass: fields[5] == "true",
			})
		case "O":
			if len(fields) != 4 {
				fail("O record needs 4 fields")
			}
			target := findStep(fields[1])
			if target == nil {
				fail("output for unknown step")
			}
			target.Outputs = append(target.Outputs, Output{
				Label: decode(fields[2]), Content: decode(fields[3]),
			})
		case "T":
			if len(fields) != 5 {
				fail("T record needs 5 fields")
			}
			target := findStep(fields[1])
			if target == nil {
				fail("artifact for unknown step")
			}
			target.Artifacts = append(target.Artifacts, Artifact{
				Kind: decode(fields[2]), Path: decode(fields[3]), SHA256: decode(fields[4]),
			})
		case "E":
			if len(fields) != 3 {
				fail("E record needs 3 fields")
			}
			target := findStep(fields[1])
			if target == nil {
				fail("end for unknown step")
			}
			target.Status = decode(fields[2])
		case "N":
			if len(fields) != 2 {
				fail("N record needs 2 fields")
			}
			receipt.Notes = append(receipt.Notes, decode(fields[1]))
		default:
			fail("unknown record type " + fields[0])
		}
	}
	if err := scanner.Err(); err != nil {
		fail(err.Error())
	}

	// A failing assertion always makes the step fail, even if the driver forgot.
	for index := range receipt.Steps {
		step := &receipt.Steps[index]
		for _, assertion := range step.Assertions {
			if !assertion.Pass && step.Status == "pass" {
				step.Status = "fail"
			}
		}
		switch step.Status {
		case "pass":
			receipt.Summary.Pass++
		case "fail":
			receipt.Summary.Fail++
		default:
			step.Status = "blocked"
			receipt.Summary.Blocked++
		}
	}
	receipt.Summary.Total = len(receipt.Steps)
	receipt.Summary.AllPass = receipt.Summary.Total > 0 &&
		receipt.Summary.Pass == receipt.Summary.Total

	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(receipt); err != nil {
		fail(err.Error())
	}
}

func fail(message string) {
	fmt.Fprintln(os.Stderr, "receiptgen: "+message)
	os.Exit(65)
}
