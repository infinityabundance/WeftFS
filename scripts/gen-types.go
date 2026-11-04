// Command gen-types turns the canonical identity schema into Rust and Go
// constants/types. It is a maintainer tool; the generated files are committed
// so both codec repositories build without a generation step.
//
// Usage (inside the Go court):
//
//	GO111MODULE=off go run scripts/gen-types.go \
//	    -schema spec/schema/identity-v1.schema \
//	    -rust WeftCodec/src/generated.rs \
//	    -go WeftCodec-Go/generated/generated.go [--check]
//
// The generator is deterministic: the same schema source always produces
// byte-identical output. `--check` compares instead of writing and exits 1 on
// drift.
package main

import (
	"bytes"
	"crypto/sha256"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

type field struct {
	key      string
	typ      string
	optional bool
}

type record struct {
	key    string
	fields []field
}

type schema struct {
	family      string
	schemas     [][2]string
	domains     [][2]string
	consts      [][2]string
	limits      [][2]string
	records     []record
	sourceBytes []byte
}

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "gen-types: "+format+"\n", args...)
	os.Exit(1)
}

func parse(path string) *schema {
	data, err := os.ReadFile(path)
	if err != nil {
		fail("cannot read schema: %v", err)
	}
	s := &schema{sourceBytes: data}
	index := map[string]int{}
	for lineNo, raw := range strings.Split(string(data), "\n") {
		line := strings.TrimSpace(raw)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.Fields(line)
		switch parts[0] {
		case "family":
			if len(parts) != 2 {
				fail("line %d: family needs one value", lineNo+1)
			}
			s.family = parts[1]
		case "schema":
			if len(parts) != 3 {
				fail("line %d: schema needs KEY VALUE", lineNo+1)
			}
			s.schemas = append(s.schemas, [2]string{parts[1], parts[2]})
		case "domain":
			if len(parts) != 3 {
				fail("line %d: domain needs KEY VALUE", lineNo+1)
			}
			s.domains = append(s.domains, [2]string{parts[1], parts[2]})
		case "const":
			if len(parts) != 3 {
				fail("line %d: const needs KEY VALUE", lineNo+1)
			}
			s.consts = append(s.consts, [2]string{parts[1], parts[2]})
		case "limit":
			if len(parts) != 3 {
				fail("line %d: limit needs KEY VALUE", lineNo+1)
			}
			s.limits = append(s.limits, [2]string{parts[1], parts[2]})
		case "record":
			if len(parts) != 2 {
				fail("line %d: record needs one key", lineNo+1)
			}
			index[parts[1]] = len(s.records)
			s.records = append(s.records, record{key: parts[1]})
		case "field":
			if len(parts) != 5 {
				fail("line %d: field needs RECORD KEY TYPE REQUIRED|OPTIONAL", lineNo+1)
			}
			at, ok := index[parts[1]]
			if !ok {
				fail("line %d: field references unknown record %q", lineNo+1, parts[1])
			}
			optional := false
			switch parts[4] {
			case "required":
			case "optional":
				optional = true
			default:
				fail("line %d: field presence must be required or optional", lineNo+1)
			}
			s.records[at].fields = append(s.records[at].fields, field{
				key: parts[2], typ: parts[3], optional: optional,
			})
		default:
			fail("line %d: unknown directive %q", lineNo+1, parts[0])
		}
	}
	if s.family == "" || len(s.schemas) == 0 || len(s.domains) == 0 || len(s.records) == 0 {
		fail("schema is incomplete (need family, schemas, domains, records)")
	}
	return s
}

func constName(key string) string {
	return strings.ToUpper(strings.ReplaceAll(key, "-", "_"))
}

func camel(key string) string {
	var b strings.Builder
	for _, part := range strings.FieldsFunc(key, func(r rune) bool { return r == '_' || r == '-' }) {
		switch part {
		case "id":
			b.WriteString("ID")
		case "uri":
			b.WriteString("URI")
		default:
			b.WriteString(strings.ToUpper(part[:1]))
			b.WriteString(part[1:])
		}
	}
	return b.String()
}

func schemaConst(key string) string {
	switch key {
	case "context-root":
		return "ContextRootSchema"
	case "causal":
		return "CausalSchema"
	case "capability":
		return "CapabilitySchema"
	default:
		return camel(key) + "Schema"
	}
}

func recordType(key string) string {
	switch key {
	case "context-root":
		return "ContextRootRecord"
	case "causal":
		return "CausalClosureRecord"
	case "capability":
		return "CapabilityScopeRecord"
	case "budget":
		return "BudgetRecord"
	default:
		return camel(key) + "Record"
	}
}

func rustType(typ string) string {
	switch typ {
	case "text":
		return "String"
	case "bytes":
		return "Vec<u8>"
	case "bytes32":
		return "[u8; 32]"
	case "bytes32_array":
		return "Vec<[u8; 32]>"
	case "text_array":
		return "Vec<String>"
	case "u64":
		return "u64"
	case "i64":
		return "i64"
	case "budget":
		return "BudgetRecord"
	default:
		fail("unknown field type %q", typ)
	}
	return ""
}

func goType(typ string) string {
	switch typ {
	case "text":
		return "string"
	case "bytes":
		return "[]byte"
	case "bytes32":
		return "[32]byte"
	case "bytes32_array":
		return "[][32]byte"
	case "text_array":
		return "[]string"
	case "u64":
		return "uint64"
	case "i64":
		return "int64"
	case "budget":
		return "BudgetRecord"
	default:
		fail("unknown field type %q", typ)
	}
	return ""
}

func hashHex(data []byte) string {
	sum := sha256.Sum256(data)
	return fmt.Sprintf("%x", sum[:])
}

func renderRust(s *schema) []byte {
	var b bytes.Buffer
	fmt.Fprintln(&b, "// @generated by scripts/gen-types.sh from spec/schema/identity-v1.schema.")
	fmt.Fprintln(&b, "// DO NOT EDIT: change the schema and regenerate; --check fails on drift.")
	fmt.Fprintf(&b, "// schema family: %s\n", s.family)
	fmt.Fprintf(&b, "// schema source sha256: %s\n\n", hashHex(s.sourceBytes))
	fmt.Fprintln(&b, "#![allow(dead_code)]")
	fmt.Fprintln(&b)
	fmt.Fprintf(&b, "pub const SCHEMA_SOURCE_SHA256: &str = \"%s\";\n", hashHex(s.sourceBytes))
	for _, item := range s.schemas {
		fmt.Fprintf(&b, "pub const %s: &str = \"%s\";\n", constName(item[0])+"_SCHEMA", item[1])
	}
	for _, item := range s.domains {
		fmt.Fprintf(&b, "pub const DOMAIN_%s: &str = \"%s\";\n", constName(item[0]), item[1])
	}
	for _, item := range s.consts {
		fmt.Fprintf(&b, "pub const %s: &str = \"%s\";\n", constName(item[0]), item[1])
	}
	for _, item := range s.limits {
		fmt.Fprintf(&b, "pub const %s: usize = %s;\n", constName(item[0]), item[1])
	}
	fmt.Fprintln(&b)
	for _, rec := range s.records {
		tname := recordType(rec.key)
		var names []string
		fmt.Fprintf(&b, "#[derive(Clone, Debug, Default, PartialEq, Eq)]\npub struct %s {\n", tname)
		for _, f := range rec.fields {
			names = append(names, f.key)
			base := rustType(f.typ)
			if f.optional {
				fmt.Fprintf(&b, "    pub %s: Option<%s>,\n", f.key, base)
			} else {
				fmt.Fprintf(&b, "    pub %s: %s,\n", f.key, base)
			}
		}
		fmt.Fprintln(&b, "}")
		fmt.Fprintf(&b, "pub const %s_FIELDS: &[&str] = &[", strings.ToUpper(strings.ReplaceAll(rec.key, "-", "_")))
		for i, n := range names {
			if i > 0 {
				fmt.Fprint(&b, ", ")
			}
			fmt.Fprintf(&b, "\"%s\"", n)
		}
		fmt.Fprintln(&b, "];\n")
	}
	fmt.Fprintln(&b, "#[cfg(test)]")
	fmt.Fprintln(&b, "mod tests {")
	fmt.Fprintln(&b, "    use super::*;")
	fmt.Fprintln(&b, "    #[test]")
	fmt.Fprintln(&b, "    fn generated_constants_match_the_runtime_context_layer() {")
	fmt.Fprintln(&b, "        assert_eq!(CONTEXT_ROOT_SCHEMA, crate::context::CONTEXT_ROOT_SCHEMA);")
	fmt.Fprintln(&b, "        assert_eq!(CAUSAL_SCHEMA, crate::context::CAUSAL_SCHEMA);")
	fmt.Fprintln(&b, "        assert_eq!(CAPABILITY_SCHEMA, crate::context::CAPABILITY_SCHEMA);")
	fmt.Fprintln(&b, "        assert_eq!(ORDERING_SELECTOR_THEN_OBJECT_ID, crate::context::ORDERING_SELECTOR_THEN_OBJECT_ID);")
	fmt.Fprintln(&b, "        assert_eq!(DEFAULT_RESOLVER, crate::context::DEFAULT_RESOLVER);")
	fmt.Fprintln(&b, "        assert_eq!(URI_PREFIX, crate::context::URI_PREFIX);")
	fmt.Fprintln(&b, "        assert_eq!(DOMAIN_CONTEXT_ROOT, crate::hash::DOMAIN_CONTEXT_ROOT);")
	fmt.Fprintln(&b, "        assert_eq!(DOMAIN_CAUSAL, crate::hash::DOMAIN_CAUSAL);")
	fmt.Fprintln(&b, "        assert_eq!(DOMAIN_CAPABILITY, crate::hash::DOMAIN_CAPABILITY);")
	fmt.Fprintln(&b, "        assert_eq!(DOMAIN_SELECTOR, crate::hash::DOMAIN_SELECTOR);")
	fmt.Fprintln(&b, "        assert_eq!(CONTEXT_ROOT_FIELDS.len(), 9);")
	fmt.Fprintln(&b, "        assert_eq!(BUDGET_FIELDS.len(), 4);")
	fmt.Fprintln(&b, "    }")
	fmt.Fprintln(&b, "}")
	return b.Bytes()
}

func renderGo(s *schema) []byte {
	var b bytes.Buffer
	fmt.Fprintln(&b, "// Code generated by scripts/gen-types.sh from spec/schema/identity-v1.schema.")
	fmt.Fprintln(&b, "// DO NOT EDIT: change the schema and regenerate; --check fails on drift.")
	fmt.Fprintf(&b, "// schema family: %s\n", s.family)
	fmt.Fprintf(&b, "// schema source sha256: %s\n\n", hashHex(s.sourceBytes))
	fmt.Fprintln(&b, "package generated")
	fmt.Fprintln(&b)
	fmt.Fprintf(&b, "const SchemaSourceSHA256 = \"%s\"\n", hashHex(s.sourceBytes))
	for _, item := range s.schemas {
		fmt.Fprintf(&b, "const %s = \"%s\"\n", schemaConst(item[0]), item[1])
	}
	for _, item := range s.domains {
		fmt.Fprintf(&b, "const Domain%s = \"%s\"\n", camel(item[0]), item[1])
	}
	for _, item := range s.consts {
		fmt.Fprintf(&b, "const %s = \"%s\"\n", camel(item[0]), item[1])
	}
	for _, item := range s.limits {
		fmt.Fprintf(&b, "const %s = %s\n", camel(item[0]), item[1])
	}
	fmt.Fprintln(&b)
	for _, rec := range s.records {
		tname := recordType(rec.key)
		var names []string
		fmt.Fprintf(&b, "type %s struct {\n", tname)
		for _, f := range rec.fields {
			names = append(names, f.key)
			base := goType(f.typ)
			if f.optional {
				fmt.Fprintf(&b, "\t%s *%s\n", camel(f.key), base)
			} else {
				fmt.Fprintf(&b, "\t%s %s\n", camel(f.key), base)
			}
		}
		fmt.Fprintln(&b, "}")
		fmt.Fprintf(&b, "var %sFields = []string{", tname)
		for i, n := range names {
			if i > 0 {
				fmt.Fprint(&b, ", ")
			}
			fmt.Fprintf(&b, "%q", n)
		}
		fmt.Fprintln(&b, "}")
		fmt.Fprintln(&b)
	}
	fmt.Fprintln(&b, "// RecordFields returns a copy of every generated record field list.")
	fmt.Fprintln(&b, "func RecordFields() map[string][]string {")
	fmt.Fprintln(&b, "\treturn map[string][]string{")
	for _, rec := range s.records {
		fmt.Fprintf(&b, "\t\t%q: append([]string(nil), %sFields...),\n", rec.key, recordType(rec.key))
	}
	fmt.Fprintln(&b, "\t}")
	fmt.Fprintln(&b, "}")
	return b.Bytes()
}

func main() {
	schemaPath := flag.String("schema", "spec/schema/identity-v1.schema", "schema source")
	rustPath := flag.String("rust", "WeftCodec/src/generated.rs", "Rust output")
	goPath := flag.String("go", "WeftCodec-Go/generated/generated.go", "Go output")
	check := flag.Bool("check", false, "compare instead of writing")
	flag.Parse()

	s := parse(*schemaPath)
	outputs := []struct {
		path string
		data []byte
	}{
		{*rustPath, renderRust(s)},
		{*goPath, renderGo(s)},
	}
	drift := false
	for _, out := range outputs {
		if *check {
			existing, err := os.ReadFile(out.path)
			if err != nil {
				fmt.Fprintf(os.Stderr, "gen-types: DRIFT %s: %v\n", out.path, err)
				drift = true
				continue
			}
			if !bytes.Equal(existing, out.data) {
				fmt.Fprintf(os.Stderr, "gen-types: DRIFT %s (regenerate with scripts/gen-types.sh)\n", out.path)
				drift = true
			}
			continue
		}
		if err := os.MkdirAll(filepath.Dir(out.path), 0o755); err != nil {
			fail("cannot create output directory: %v", err)
		}
		if err := os.WriteFile(out.path, out.data, 0o644); err != nil {
			fail("cannot write %s: %v", out.path, err)
		}
		fmt.Printf("wrote %s (%d bytes, schema sha256 %s)\n", out.path, len(out.data), hashHex(s.sourceBytes))
	}
	if *check {
		if drift {
			os.Exit(1)
		}
		fmt.Printf("gen-types: OK (schema sha256 %s)\n", hashHex(s.sourceBytes))
	}
}
