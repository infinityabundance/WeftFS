// Semantic receipt comparison for the F3 demos.
//
// Wall-clock fields, run ids, and run-specific scratch paths differ between
// runs by design. This comparator keeps the semantic fields: step ids, titles,
// statuses, assertions (name/expected/actual/pass), observed outputs, notes,
// environment, repositories, and the summary. Commands and artifact paths are
// normalized away because they embed the run id.
//
// Usage: democmp A.json B.json
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"reflect"
)

type normalized struct {
	Schema       string            `json:"schema"`
	Environment  map[string]string `json:"environment"`
	Repositories []any             `json:"repositories"`
	Steps        []any             `json:"steps"`
	Notes        []any             `json:"notes"`
	Summary      any               `json:"summary"`
}

func load(path string) map[string]any {
	raw, err := os.ReadFile(path)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(66)
	}
	var value map[string]any
	if err := json.Unmarshal(raw, &value); err != nil {
		fmt.Fprintf(os.Stderr, "democmp: %s: %v\n", path, err)
		os.Exit(65)
	}
	return value
}

func list(value any) []any {
	if items, ok := value.([]any); ok {
		return items
	}
	return []any{}
}

func text(value any) string {
	if value, ok := value.(string); ok {
		return value
	}
	return ""
}

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: democmp A.json B.json")
		os.Exit(64)
	}
	left := load(os.Args[1])
	right := load(os.Args[2])
	normalize := func(value map[string]any) normalized {
		steps := make([]any, 0)
		if items, ok := value["steps"].([]any); ok {
			for _, item := range items {
				step, _ := item.(map[string]any)
				steps = append(steps, map[string]any{
					"id":         step["id"],
					"title":      step["title"],
					"status":     step["status"],
					"notes":      step["notes"],
					"assertions": step["assertions"],
					"outputs":    step["outputs"],
				})
			}
		}
		environment := map[string]string{}
		if items, ok := value["environment"].(map[string]any); ok {
			for key, raw := range items {
				if text, ok := raw.(string); ok {
					environment[key] = text
				}
			}
		}
		return normalized{
			Schema:       text(value["schema"]),
			Environment:  environment,
			Repositories: list(value["repositories"]),
			Steps:        steps,
			Notes:        list(value["notes"]),
			Summary:      value["summary"],
		}
	}
	a := normalize(left)
	b := normalize(right)
	if reflect.DeepEqual(a, b) {
		fmt.Println("semantic_match=true")
		return
	}
	fmt.Println("semantic_match=false")
	leftJSON, _ := json.MarshalIndent(a, "", "  ")
	rightJSON, _ := json.MarshalIndent(b, "", "  ")
	_ = os.WriteFile("/tmp/democmp-left.json", leftJSON, 0o644)
	_ = os.WriteFile("/tmp/democmp-right.json", rightJSON, 0o644)
	fmt.Fprintln(os.Stderr, "left:\n"+string(leftJSON))
	fmt.Fprintln(os.Stderr, "right:\n"+string(rightJSON))
	os.Exit(1)
}
