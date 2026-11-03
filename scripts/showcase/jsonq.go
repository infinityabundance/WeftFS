//! Tiny JSON field extractor for the showcase script (stdlib only).
//!
//! Usage: jsonq FILE PATH
//!   PATH is dot-separated; numeric segments index arrays.
//!   Strings print raw, booleans/numbers print their literal form, and
//!   objects/arrays print compact JSON. Missing paths exit 66.

package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"
)

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: jsonq FILE PATH")
		os.Exit(64)
	}
	raw, err := os.ReadFile(os.Args[1])
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(66)
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	var value any
	if err := decoder.Decode(&value); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(65)
	}
	path := os.Args[2]
	wantLen := false
	if strings.HasSuffix(path, "#len") {
		wantLen = true
		path = strings.TrimSuffix(path, "#len")
	}
	if path != "" && path != "." {
		for _, segment := range strings.Split(path, ".") {
			switch node := value.(type) {
			case map[string]any:
				next, ok := node[segment]
				if !ok {
					fmt.Fprintf(os.Stderr, "jsonq: no key %q in %s\n", segment, path)
					os.Exit(66)
				}
				value = next
			case []any:
				index, err := strconv.Atoi(segment)
				if err != nil || index < 0 || index >= len(node) {
					fmt.Fprintf(os.Stderr, "jsonq: bad array index %q in %s\n", segment, path)
					os.Exit(66)
				}
				value = node[index]
			default:
				fmt.Fprintf(os.Stderr, "jsonq: cannot descend into %T at %q\n", value, segment)
				os.Exit(66)
			}
		}
	}
	if wantLen {
		switch node := value.(type) {
		case []any:
			fmt.Println(len(node))
		case string:
			fmt.Println(len(node))
		case map[string]any:
			fmt.Println(len(node))
		default:
			fmt.Fprintf(os.Stderr, "jsonq: %s is not a sized value\n", os.Args[2])
			os.Exit(66)
		}
		return
	}
	switch node := value.(type) {
	case string:
		fmt.Println(node)
	case bool:
		if node {
			fmt.Println("true")
		} else {
			fmt.Println("false")
		}
	case json.Number:
		fmt.Println(node.String())
	case nil:
		fmt.Println("null")
	default:
		encoded, err := json.Marshal(node)
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(65)
		}
		fmt.Println(string(encoded))
	}
}
