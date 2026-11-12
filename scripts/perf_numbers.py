#!/usr/bin/env python3
"""Generate and check every published performance number from its receipt.

The manifest (`docs/perf-numbers.json`) is the single source. Each number names
the receipt file it comes from, the JSON path (or a bounded expression over
other numbers), and the stamp and revision that receipt carries. The generator
rewrites only the text between sentinel comments:

    <!-- BEGIN GENERATED:<block-id> (scripts/perf_numbers.py) -->
    ...
    <!-- END GENERATED:<block-id> -->

Everything outside a sentinel region is left byte-identical. `--check` fails
closed when a region is stale, when a referenced receipt is missing, when a
declared stamp or revision no longer matches the receipt, or when a manifest
number has no consumer. `--list` prints the mapping for audit.

Host-runnable, standard library only. It reads files, never the network.
"""

from __future__ import annotations

import argparse
import ast
import json
import os
import re
import sys

SENTINEL_BEGIN = "<!-- BEGIN GENERATED:%s (scripts/perf_numbers.py) -->"
SENTINEL_END = "<!-- END GENERATED:%s -->"
SENTINEL_RE = re.compile(
    r"<!-- BEGIN GENERATED:([A-Za-z0-9_.:-]+) \(scripts/perf_numbers\.py\) -->"
)
PLACEHOLDER_RE = re.compile(r"\{\{([A-Za-z0-9_.:\-|]+)\}\}")
EXPR_REF_RE = re.compile(r"\{([A-Za-z0-9_.:-]+)\}")


class Failure(Exception):
    pass


# --------------------------------------------------------------------------
# formatting


def fmt_value(value, spec):
    """Format one number. `spec` is `int`, `fixed:N`, `pct:N`, `ratio:N`,
    `times:N` or `text`."""
    if spec in (None, "text"):
        if isinstance(value, str):
            return value
        if isinstance(value, float) and value.is_integer():
            return str(int(value))
        return str(value)
    if isinstance(value, str):
        try:
            value = float(value) if ("." in value or "e" in value.lower()) else int(value)
        except ValueError:
            raise Failure("value %r is not numeric for format %r" % (value, spec))
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise Failure("value %r is not numeric for format %r" % (value, spec))
    if spec == "int":
        return "{:,.0f}".format(value)
    kind, _, digits = spec.partition(":")
    if kind in ("fixed", "pct", "ratio", "times"):
        if not digits.isdigit():
            raise Failure("bad format %r" % spec)
        return ("{:,.%df}" % int(digits)).format(value)
    if kind == "trunc":
        if not digits.isdigit():
            raise Failure("bad format %r" % spec)
        digits = int(digits)
        scale = 10 ** digits
        truncated = int(value * scale) / scale if value >= 0 else -int(-value * scale) / scale
        return ("{:,.%df}" % digits).format(truncated)
    raise Failure("unknown format %r" % spec)


def unit_suffix(unit):
    if not unit:
        return ""
    if unit in ("%", "x", "×"):
        return unit
    return " " + unit


# --------------------------------------------------------------------------
# JSON navigation


def split_path(path):
    return [part for part in path.split(".") if part != ""]


def get_path(obj, path, where):
    cur = obj
    for part in split_path(path):
        if isinstance(cur, dict):
            if part not in cur:
                raise Failure("%s: missing key %r in path %r" % (where, part, path))
            cur = cur[part]
        elif isinstance(cur, list):
            if not part.isdigit() or int(part) >= len(cur):
                raise Failure("%s: bad index %r in path %r" % (where, part, path))
            cur = cur[int(part)]
        else:
            raise Failure("%s: path %r walks into a scalar at %r" % (where, path, part))
    return cur


def select_from(obj, spec, where):
    """Resolve `list` + `where` + `path` (a single matching element)."""
    items = get_path(obj, spec["list"], where)
    if not isinstance(items, list):
        raise Failure("%s: %r is not a list" % (where, spec["list"]))
    predicate = spec.get("where", {})
    matches = []
    for item in items:
        if isinstance(item, dict) and all(item.get(k) == v for k, v in predicate.items()):
            matches.append(item)
    if len(matches) != 1:
        raise Failure(
            "%s: selector %r over %r matched %d rows (need exactly 1)"
            % (where, predicate, spec["list"], len(matches))
        )
    rest = spec.get("path")
    return matches[0] if rest in (None, "") else get_path(matches[0], rest, where)


# --------------------------------------------------------------------------
# expressions


_ALLOWED_NODES = (
    ast.Expression,
    ast.BinOp,
    ast.UnaryOp,
    ast.Constant,
    ast.Add,
    ast.Sub,
    ast.Mult,
    ast.Div,
    ast.USub,
    ast.UAdd,
    ast.Load,
)


def eval_expr(expr, values, where):
    substituted = EXPR_REF_RE.sub(lambda m: repr(float(values[m.group(1)])), expr)
    try:
        tree = ast.parse(substituted, mode="eval")
    except SyntaxError as exc:
        raise Failure("%s: cannot parse expression %r: %s" % (where, expr, exc))
    for node in ast.walk(tree):
        if not isinstance(node, _ALLOWED_NODES):
            raise Failure(
                "%s: expression %r uses forbidden syntax %s"
                % (where, expr, type(node).__name__)
            )
    try:
        return eval(compile(tree, "<perf-numbers>", "eval"), {"__builtins__": {}}, {})
    except ZeroDivisionError:
        raise Failure("%s: expression %r divides by zero" % (where, expr))


# --------------------------------------------------------------------------
# manifest


class Manifest:
    def __init__(self, root, data):
        self.root = root
        self.data = data
        self.numbers = {}
        self.order = []
        self.groups = []
        self.blocks = []
        self.block_by_id = {}

    def load(self):
        if self.data.get("schema") != "weft.perf_numbers.v1":
            raise Failure("manifest schema must be weft.perf_numbers.v1")
        for group in self.data.get("groups", []):
            self.groups.append(group)
        for entry in self.data.get("numbers", []):
            key = entry.get("key")
            if not key:
                raise Failure("manifest number without a key")
            if key in self.numbers:
                raise Failure("duplicate number key %r" % key)
            for field in ("label", "unit", "group", "format"):
                if field not in entry:
                    raise Failure("%s: missing field %r" % (key, field))
            self.numbers[key] = entry
            self.order.append(key)
        for block in self.data.get("blocks", []):
            bid = block.get("id")
            if not bid:
                raise Failure("manifest block without an id")
            if bid in self.block_by_id:
                raise Failure("duplicate block id %r" % bid)
            self.block_by_id[bid] = block
            self.blocks.append(block)

    # -- receipts ---------------------------------------------------------

    def receipt_path(self, rel):
        path = os.path.join(self.root, rel)
        if not os.path.isfile(path):
            raise Failure("missing receipt %s" % rel)
        return path

    def read_receipt(self, rel):
        with open(self.receipt_path(rel), "r", encoding="utf-8") as handle:
            return json.load(handle)

    def check_provenance(self, where, rel, stamp_path, stamp, revision_path, revision, digest_path, digest):
        receipt = self.read_receipt(rel)
        if stamp_path:
            actual = get_path(receipt, stamp_path, where)
            if stamp is not None and actual != stamp:
                raise Failure(
                    "%s: receipt %s records %s=%r, manifest declares %r"
                    % (where, rel, stamp_path, actual, stamp)
                )
        elif stamp is not None:
            raise Failure("%s: stamp declared but stamp_path is null" % where)
        if revision_path:
            actual = get_path(receipt, revision_path, where)
            if revision is not None and actual != revision:
                raise Failure(
                    "%s: receipt %s records %s=%r, manifest declares %r"
                    % (where, rel, revision_path, actual, revision)
                )
        elif revision is not None:
            raise Failure("%s: revision declared but revision_path is null" % where)
        if digest_path:
            actual = get_path(receipt, digest_path, where)
            if digest is not None and actual != digest:
                raise Failure(
                    "%s: receipt %s records %s=%r, manifest declares %r"
                    % (where, rel, digest_path, actual, digest)
                )
        return receipt

    # -- values -----------------------------------------------------------

    def resolve_numbers(self):
        values = {}
        # source numbers first
        for key in self.order:
            entry = self.numbers[key]
            has_path = "path" in entry
            has_list = "list" in entry
            has_expr = "expr" in entry
            has_count = "count_of" in entry
            has_count_path = "count_path" in entry
            if sum((has_path, has_list, has_expr, has_count, has_count_path)) != 1:
                raise Failure(
                    "%s: exactly one of path, list, expr, count_of, count_path is required" % key
                )
            if has_expr:
                continue
            if has_count_path:
                rel = entry.get("receipt")
                if not rel:
                    raise Failure("%s: count_path needs a receipt" % key)
                receipt = self.check_provenance(
                    key,
                    rel,
                    entry.get("stamp_path"),
                    entry.get("stamp"),
                    entry.get("revision_path"),
                    entry.get("revision"),
                    entry.get("digest_path"),
                    entry.get("digest"),
                )
                container = get_path(receipt, entry["count_path"], key)
                if not isinstance(container, (list, dict)):
                    raise Failure("%s: %r is not a container" % (key, entry["count_path"]))
                values[key] = len(container)
                continue
            if has_count:
                rel = entry["count_of"]
                path = os.path.join(self.root, rel)
                if not os.path.isfile(path):
                    raise Failure("%s: missing file %s" % (key, rel))
                with open(path, "r", encoding="utf-8") as handle:
                    data = json.load(handle)
                if not isinstance(data, list):
                    raise Failure("%s: %s is not a JSON array" % (key, rel))
                values[key] = len(data)
                continue
            rel = entry.get("receipt")
            if not rel:
                raise Failure("%s: a path/list source needs a receipt" % key)
            receipt = self.check_provenance(
                key,
                rel,
                entry.get("stamp_path"),
                entry.get("stamp"),
                entry.get("revision_path"),
                entry.get("revision"),
                entry.get("digest_path"),
                entry.get("digest"),
            )
            if has_list:
                selector = {
                    "list": entry["list"],
                    "where": entry.get("where", {}),
                    "path": entry.get("select_path"),
                }
                value = select_from(receipt, selector, key)
            else:
                value = get_path(receipt, entry["path"], key)
            if isinstance(value, bool):
                raise Failure("%s: boolean %r is not a published number" % (key, value))
            if not isinstance(value, (int, float, str)):
                raise Failure("%s: value %r is not a scalar" % (key, value))
            values[key] = value
        # derived numbers, in declaration order, may reference source numbers
        for key in self.order:
            entry = self.numbers[key]
            if "expr" not in entry:
                continue
            refs = EXPR_REF_RE.findall(entry["expr"])
            for ref in refs:
                if ref not in values:
                    raise Failure(
                        "%s: expression references %r which is not a resolved number"
                        % (key, ref)
                    )
            values[key] = eval_expr(entry["expr"], values, key)
            entry["_derived_from"] = sorted(set(refs))
        self.values = values

    def leaves(self, key, seen=None):
        """Source entries a number depends on (itself when it has a receipt)."""
        if seen is None:
            seen = set()
        entry = self.numbers[key]
        if "expr" not in entry:
            if "receipt" in entry or "count_of" in entry:
                return [entry]
            return []
        out = []
        for ref in self.numbers[key].get("_derived_from", []):
            if ref in seen:
                continue
            seen.add(ref)
            out.extend(self.leaves(ref, seen))
        return out

    def display(self, key, field=None):
        entry = self.numbers[key]
        value = self.values[key]
        if field is None:
            return fmt_value(value, entry["format"])
        if field == "unit":
            return entry["unit"]
        if field == "label":
            return entry["label"]
        if field == "receipt":
            sources = self.leaves(key)
            names = []
            for source in sources:
                name = source.get("receipt") or source.get("count_of")
                if name and name not in names:
                    names.append(name)
            return ", ".join(names) if names else "—"
        if field == "stamp":
            stamps = {s.get("stamp") for s in self.leaves(key) if s.get("stamp")}
            if not stamps:
                return "—"
            return stamps.pop() if len(stamps) == 1 else "mixed"
        if field == "revision":
            revs = {s.get("revision") for s in self.leaves(key) if s.get("revision")}
            if not revs:
                return "—"
            if len(revs) == 1:
                return revs.pop()[:12]
            return "mixed"
        if field == "group":
            return entry["group"]
        raise Failure("%s: unknown display field %r" % (key, field))

    def substitute(self, template, where):
        def repl(match):
            token = match.group(1)
            key, _, field = token.partition("|")
            if key not in self.numbers:
                raise Failure("%s: unknown number %r in template" % (where, key))
            return self.display(key, field or None)

        return PLACEHOLDER_RE.sub(repl, template)

    # -- rendering --------------------------------------------------------

    def render(self, block):
        bid = block["id"]
        kind = block.get("kind", "lines")
        if kind == "lines":
            return self.render_lines(block)
        if kind == "catalog":
            return self.render_catalog(block)
        if kind == "matrix":
            return self.render_matrix(block)
        if kind == "frontier":
            return self.render_frontier(block)
        raise Failure("%s: unknown block kind %r" % (bid, kind))

    def render_lines(self, block):
        bid = block["id"]
        out = []
        for row in block.get("rows", []):
            template = row["template"] if isinstance(row, dict) else row
            keys = row.get("keys", []) if isinstance(row, dict) else []
            where = "%s (%s)" % (bid, ",".join(keys) or "-")
            out.append(self.substitute(template, where))
        joiner = " " if block.get("inline") else "\n"
        return joiner.join(out)

    def render_catalog(self, block):
        bid = block["id"]
        group = block.get("group")
        if group not in [g["id"] for g in self.groups]:
            raise Failure("%s: unknown group %r" % (bid, group))
        lines = [
            "| key | value | unit | what it measures | receipt | stamp | revision |",
            "|---|---:|---|---|---|---|---|",
        ]
        for key in self.order:
            entry = self.numbers[key]
            if entry["group"] != group:
                continue
            unit = entry["unit"]
            lines.append(
                "| `%s` | %s | %s | %s | `%s` | %s | %s |"
                % (
                    key,
                    self.display(key),
                    unit or "—",
                    entry["label"],
                    self.display(key, "receipt"),
                    self.display(key, "stamp"),
                    self.display(key, "revision"),
                )
            )
        return "\n".join(lines)

    def render_matrix(self, block):
        bid = block["id"]
        spec = block["matrix"]
        rel = spec["receipt"]
        receipt = self.check_provenance(
            bid,
            rel,
            spec.get("stamp_path"),
            spec.get("stamp"),
            spec.get("revision_path"),
            spec.get("revision"),
            spec.get("digest_path"),
            spec.get("digest"),
        )
        table = get_path(receipt, spec["array"], bid)
        if not isinstance(table, dict):
            raise Failure("%s: %r is not an object" % (bid, spec["array"]))
        verdicts = spec.get("verdicts", {})
        notes = spec.get("notes", {})
        columns = spec["columns"]
        header = "| " + " | ".join(c["header"] for c in columns) + " |"
        align = "|" + "|".join(c.get("align", "---") for c in columns) + "|"
        lines = [header, align]
        for name in spec["order"]:
            if name not in table:
                raise Failure("%s: config %r missing from %s" % (bid, name, rel))
            row = table[name]
            cells = []
            for column in columns:
                if "text" in column:
                    text = column["text"]
                    text = text.replace("{name}", name).replace(
                        "{verdict}", verdicts.get(name, "")
                    ).replace("{note}", notes.get(name, ""))
                    cells.append(self.substitute(text, "%s/%s" % (bid, name)))
                else:
                    value = get_path(row, column["path"], "%s/%s" % (bid, name))
                    cells.append(fmt_value(value, column.get("format")))
            lines.append("| " + " | ".join(cells) + " |")
        return "\n".join(lines)

    def render_frontier(self, block):
        """Render the non-dominated set of a receipt table on two minimised
        axes. A row is dominated when another row is no worse on both axes and
        strictly better on at least one. `exclude` removes rows that are not
        eligible for policy reasons; the exclusion is printed, never silent."""
        bid = block["id"]
        spec = block["frontier"]
        rel = spec["receipt"]
        receipt = self.check_provenance(
            bid,
            rel,
            spec.get("stamp_path"),
            spec.get("stamp"),
            spec.get("revision_path"),
            spec.get("revision"),
            spec.get("digest_path"),
            spec.get("digest"),
        )
        table = get_path(receipt, spec["array"], bid)
        if not isinstance(table, dict):
            raise Failure("%s: %r is not an object" % (bid, spec["array"]))
        excluded = set(spec.get("exclude", []))
        rows = []
        for name, row in table.items():
            if name in excluded:
                continue
            wall = get_path(row, spec["wall_path"], "%s/%s" % (bid, name))
            size = get_path(row, spec["bytes_path"], "%s/%s" % (bid, name))
            if not isinstance(wall, (int, float)) or not isinstance(size, (int, float)):
                raise Failure("%s/%s: axes are not numeric" % (bid, name))
            rows.append((name, float(wall), float(size)))
        if not rows:
            raise Failure("%s: no rows after exclusion" % bid)
        frontier = []
        for name, wall, size in rows:
            dominated = any(
                (other_wall <= wall and other_size <= size)
                and (other_wall < wall or other_size < size)
                for other_name, other_wall, other_size in rows
                if other_name != name
            )
            if not dominated:
                frontier.append((name, wall, size))
        frontier.sort(key=lambda item: (item[1], item[2], item[0]))
        wall_fmt = spec.get("format_wall", "fixed:1")
        size_fmt = spec.get("format_bytes", "int")
        parts = [
            "**%s** (%s ms, %s B)"
            % (name, fmt_value(wall, wall_fmt), fmt_value(size, size_fmt))
            for name, wall, size in frontier
        ]
        out = spec.get("prefix", "") + ", ".join(parts) + spec.get("suffix", ".")
        if excluded:
            out += " " + spec.get("excluded_note", "Excluded as not policy-eligible: %s." % ", ".join(sorted(excluded)))
        return out

    # -- sentinels --------------------------------------------------------

    def region_re(self, bid):
        start = re.escape(SENTINEL_BEGIN % bid)
        end = re.escape(SENTINEL_END % bid)
        return re.compile(start + r"(.*?)" + end, re.DOTALL)

    def apply(self, check_only):
        problems = []
        changed = []
        block_consumers = {bid: 0 for bid in self.block_by_id}

        for block in self.blocks:
            rel = block.get("file")
            if not rel:
                raise Failure("%s: block without a file" % block["id"])
            path = os.path.join(self.root, rel)
            if not os.path.isfile(path):
                raise Failure("%s: missing file %s" % (block["id"], rel))
            with open(path, "r", encoding="utf-8") as handle:
                text = handle.read()
            pattern = self.region_re(block["id"])
            found = pattern.findall(text)
            if len(found) != 1:
                raise Failure(
                    "%s: expected exactly one generated region in %s, found %d"
                    % (block["id"], rel, len(found))
                )
            block_consumers[block["id"]] += 1
            rendered = self.render(block)
            if block.get("inline"):
                expected = (SENTINEL_BEGIN % block["id"]) + " " + rendered + " " + (
                    SENTINEL_END % block["id"]
                )
            else:
                expected = (SENTINEL_BEGIN % block["id"]) + "\n" + rendered + "\n" + (
                    SENTINEL_END % block["id"]
                )
            match = pattern.search(text)
            actual = match.group(0)
            actual_body = match.group(1)
            if expected != actual:
                changed.append(rel)
                if check_only:
                    problems.extend(
                        self.describe_staleness(block, rel, rendered, actual_body)
                    )
                else:
                    text = pattern.sub(lambda _m: expected, text, count=1)
                    with open(path, "w", encoding="utf-8") as handle:
                        handle.write(text)

        # every number must be consumed by at least one block
        used = set()
        for block in self.blocks:
            for token in self.block_tokens(block):
                used.add(token.partition("|")[0])
        for key in self.order:
            if key not in used:
                problems.append("UNCONSUMED number %r is in the manifest but no block prints it" % key)
        for bid, count in block_consumers.items():
            if count == 0:
                problems.append("UNCONSUMED block %r is in the manifest but no file contains it" % bid)

        # every sentinel in the tree must have a manifest block
        for rel in self.tracked_markdown():
            path = os.path.join(self.root, rel)
            try:
                with open(path, "r", encoding="utf-8") as handle:
                    text = handle.read()
            except (UnicodeDecodeError, OSError):
                continue
            for bid in SENTINEL_RE.findall(text):
                if bid not in self.block_by_id:
                    problems.append(
                        "UNKNOWN block id %r appears in %s but not in the manifest" % (bid, rel)
                    )

        if check_only:
            return problems, changed
        return problems, changed

    def describe_staleness(self, block, rel, rendered, actual):
        out = []
        exp_lines = ("\n" + rendered + "\n").splitlines()
        act_lines = actual.splitlines()
        for token in sorted(self.block_tokens(block)):
            key, _, field = token.partition("|")
            value = self.display(key, field or None)
            if value and value not in actual:
                out.append(
                    "STALE KEY %s: expected %r (from %s %s) not present in %s"
                    % (
                        key,
                        value,
                        self.display(key, "receipt"),
                        self.display(key, "stamp"),
                        rel,
                    )
                )
        # line-level diff, attributed to the block
        for index in range(max(len(exp_lines), len(act_lines))):
            exp = exp_lines[index] if index < len(exp_lines) else "<missing line>"
            act = act_lines[index] if index < len(act_lines) else "<missing line>"
            if exp != act:
                out.append("STALE LINE %s line %d:\n  expected: %s\n  actual:   %s" % (rel, index + 1, exp, act))
                break
        return out

    def block_tokens(self, block):
        tokens = []
        kind = block.get("kind", "lines")
        if kind == "lines":
            for row in block.get("rows", []):
                template = row["template"] if isinstance(row, dict) else row
                tokens.extend(PLACEHOLDER_RE.findall(template))
        elif kind == "matrix":
            for column in block["matrix"]["columns"]:
                if "text" in column:
                    tokens.extend(PLACEHOLDER_RE.findall(column["text"]))
        elif kind == "catalog":
            group = block.get("group")
            tokens.extend(k for k in self.order if self.numbers[k]["group"] == group)
        return tokens

    def tracked_markdown(self):
        index = os.path.join(self.root, ".git", "index")
        if not os.path.exists(index):
            # not a git checkout: only the declared files
            return sorted({b["file"] for b in self.blocks if b.get("file")})
        import subprocess

        proc = subprocess.run(
            ["git", "-C", self.root, "ls-files", "*.md"],
            capture_output=True,
            text=True,
            check=False,
        )
        files = [line for line in proc.stdout.splitlines() if line.endswith(".md")]
        declared = {b["file"] for b in self.blocks if b.get("file")}
        return sorted(set(files) | declared)

    def listing(self):
        groups = {g["id"]: g.get("title", g["id"]) for g in self.groups}
        lines = []
        for key in self.order:
            entry = self.numbers[key]
            source = entry.get("receipt")
            if source:
                source = "%s [%s]" % (source, entry.get("path") or ("list:%s" % entry.get("list") if entry.get("list") else "count:%s" % entry.get("count_path")))
            elif "count_of" in entry:
                source = "count of %s" % entry["count_of"]
            else:
                source = "derived from %s" % ", ".join(entry.get("_derived_from", []))
            rev = entry.get("revision")
            rev = rev[:12] if isinstance(rev, str) else "(none recorded in receipt)"
            lines.append(
                "%-34s %-28s %-13s %-8s %s\n    %s\n    stamp %s  revision %s"
                % (
                    key,
                    entry["label"],
                    entry["format"],
                    entry["unit"] or "-",
                    groups.get(entry["group"], entry["group"]),
                    source,
                    entry.get("stamp") or "-",
                    rev,
                )
            )
        return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", help="repository root")
    parser.add_argument("--manifest", default="docs/perf-numbers.json")
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--check", action="store_true", help="fail on drift")
    group.add_argument("--list", action="store_true", help="print the mapping")
    args = parser.parse_args(argv)

    root = os.path.abspath(args.root)
    manifest_path = args.manifest
    if not os.path.isabs(manifest_path):
        manifest_path = os.path.join(root, manifest_path)
    if not os.path.isfile(manifest_path):
        print("perf_numbers: missing manifest %s" % args.manifest, file=sys.stderr)
        return 2
    with open(manifest_path, "r", encoding="utf-8") as handle:
        data = json.load(handle)

    manifest = Manifest(root, data)
    try:
        manifest.load()
        manifest.resolve_numbers()
        if args.list:
            print(manifest.listing())
            return 0
        problems, changed = manifest.apply(check_only=args.check)
    except Failure as exc:
        print("perf_numbers: %s" % exc, file=sys.stderr)
        return 1

    if args.check:
        if problems or changed:
            for line in problems:
                print(line, file=sys.stderr)
            for rel in sorted(set(changed)):
                if not any(rel in p for p in problems):
                    print("STALE %s: a generated region does not match its receipt" % rel, file=sys.stderr)
            print(
                "perf_numbers: FAIL (%d problem(s), %d stale file(s))"
                % (len(problems), len(set(changed))),
                file=sys.stderr,
            )
            return 1
        print(
            "perf_numbers: OK (%d numbers, %d blocks)"
            % (len(manifest.numbers), len(manifest.blocks))
        )
        return 0

    if problems:
        for line in problems:
            print("perf_numbers: %s" % line, file=sys.stderr)
        return 1
    print(
        "perf_numbers: wrote %d block(s) across %d file(s); %d numbers"
        % (len(manifest.blocks), len(set(changed)), len(manifest.numbers))
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
