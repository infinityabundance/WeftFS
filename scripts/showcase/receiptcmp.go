//! Compare two showcase receipts for semantic determinism.
//!
//! Only the wall-clock fields (`generated_at` and the `run` timing block) are
//! excluded. Everything else -- environment, revisions, fixtures, commands,
//! assertions, observed outputs, and step statuses -- must match exactly.
//!
//! Usage: receiptcmp A.json B.json

package main

import (
	"encoding/json"
	"fmt"
	"os"
	"reflect"
)

func load(path string) map[string]any {
	raw, err := os.ReadFile(path)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(66)
	}
	var value map[string]any
	if err := json.Unmarshal(raw, &value); err != nil {
		fmt.Fprintf(os.Stderr, "receiptcmp: %s: %v\n", path, err)
		os.Exit(65)
	}
	delete(value, "generated_at")
	delete(value, "run")
	return value
}

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: receiptcmp A.json B.json")
		os.Exit(64)
	}
	left := load(os.Args[1])
	right := load(os.Args[2])
	if reflect.DeepEqual(left, right) {
		fmt.Println("semantic_match=true")
		return
	}
	fmt.Println("semantic_match=false")
	leftJSON, _ := json.MarshalIndent(left, "", "  ")
	rightJSON, _ := json.MarshalIndent(right, "", "  ")
	fmt.Fprintln(os.Stderr, "left:\n"+string(leftJSON))
	fmt.Fprintln(os.Stderr, "right:\n"+string(rightJSON))
	os.Exit(1)
}
