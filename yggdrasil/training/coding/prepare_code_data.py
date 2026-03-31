#!/usr/bin/env python3
"""SFT Dataset Preparation for Yggdrasil Code Specialists.

Walks the Yggdrasil crate directory and generates four types of training
data for the grokked specialist models:

1. **Review pairs** — code with intentional issues + expected review feedback
2. **Test pairs** — function signatures → matching test implementations
3. **Code gen pairs** — task description → implementation (for eval baseline)

Output: JSONL files in Liquid AI chat format:
  {"messages": [{"role":"system","content":"..."}, {"role":"user","content":"..."}, {"role":"assistant","content":"..."}]}

Usage:
    python prepare_code_data.py --crate-dir ../../crates --output-dir ./data
    python prepare_code_data.py --crate-dir ../../crates --output-dir ./data --augment
"""

import argparse
import json
import os
import re
from pathlib import Path
from dataclasses import dataclass

CRATE_DIR = Path(__file__).parent.parent.parent / "crates"

# ─────────────────────────────────────────────────────────────────
# Extraction helpers
# ─────────────────────────────────────────────────────────────────

@dataclass
class RustFunction:
    """Extracted Rust function with metadata."""
    name: str
    signature: str
    body: str
    doc_comment: str
    file_path: str
    is_async: bool
    is_pub: bool
    is_test: bool
    parent_impl: str  # e.g., "impl AppState" or ""


def extract_functions(source: str, file_path: str) -> list[RustFunction]:
    """Extract function definitions from Rust source code."""
    functions = []

    # Pattern: optional doc comments + optional pub + optional async + fn name
    pattern = re.compile(
        r'(?P<doc>(?:\s*///[^\n]*\n)*)'        # doc comments
        r'\s*(?P<pub>pub(?:\(crate\))?\s+)?'     # optional pub
        r'(?P<async>async\s+)?'                  # optional async
        r'fn\s+(?P<name>\w+)'                    # fn name
        r'(?P<sig>[^{]*)'                        # signature (params + return)
        r'\{',                                   # opening brace
        re.MULTILINE,
    )

    for m in pattern.finditer(source):
        name = m.group("name")
        doc = m.group("doc").strip()
        is_pub = bool(m.group("pub"))
        is_async = bool(m.group("async"))
        sig_parts = m.group("sig").strip()
        is_test = "test" in name or "#[test]" in source[max(0, m.start()-50):m.start()]

        # Extract the full signature line
        prefix = "pub " if is_pub else ""
        async_kw = "async " if is_async else ""
        signature = f"{prefix}{async_kw}fn {name}{sig_parts}"

        # Extract body by brace matching
        start = m.end() - 1  # position of opening {
        depth = 0
        end = start
        for i in range(start, len(source)):
            if source[i] == '{':
                depth += 1
            elif source[i] == '}':
                depth -= 1
                if depth == 0:
                    end = i + 1
                    break

        body = source[start:end] if end > start else "{}"

        # Find parent impl block
        parent = ""
        impl_match = re.search(
            r'impl(?:<[^>]*>)?\s+(\w+(?:<[^>]*>)?)',
            source[max(0, m.start()-500):m.start()],
        )
        if impl_match:
            parent = f"impl {impl_match.group(1)}"

        functions.append(RustFunction(
            name=name,
            signature=signature.strip(),
            body=body,
            doc_comment=doc,
            file_path=str(file_path),
            is_async=is_async,
            is_pub=is_pub,
            is_test=is_test,
            parent_impl=parent,
        ))

    return functions


def extract_structs(source: str) -> list[tuple[str, str]]:
    """Extract struct definitions. Returns [(name, full_definition)]."""
    structs = []
    pattern = re.compile(
        r'((?:\s*///[^\n]*\n)*'             # doc comments
        r'\s*#\[derive\([^\]]*\)\]\s*\n'    # derive attribute
        r'\s*pub\s+struct\s+(\w+)[^{]*\{)',  # struct name
        re.MULTILINE,
    )

    for m in pattern.finditer(source):
        name = m.group(2)
        start = m.start()
        # Find closing brace
        brace_start = m.end() - 1
        depth = 0
        end = brace_start
        for i in range(brace_start, len(source)):
            if source[i] == '{':
                depth += 1
            elif source[i] == '}':
                depth -= 1
                if depth == 0:
                    end = i + 1
                    break
        structs.append((name, source[start:end].strip()))

    return structs


# ─────────────────────────────────────────────────────────────────
# Dataset generators
# ─────────────────────────────────────────────────────────────────

REVIEWER_SYSTEM = (
    "You are a Rust code reviewer for the Yggdrasil AI homelab project. "
    "Review code for bugs, convention violations, security issues, and hardcoded values. "
    'Respond with JSON: {"issues": [{"severity": "high|medium|low", "description": "..."}], '
    '"verdict": "LGTM|NEEDS_FIXES"}'
)

TESTER_SYSTEM = (
    "You are a Rust test generator for the Yggdrasil AI homelab project. "
    "Given a function signature and context, generate comprehensive #[tokio::test] or #[test] tests. "
    "Use assert_eq!, assert!, and descriptive test names in snake_case."
)

CODER_SYSTEM = (
    "You are a Rust code generator for the Yggdrasil AI homelab project. "
    "Generate idiomatic Rust code following Yggdrasil conventions: axum handlers, "
    "serde structs, thiserror enums, tracing instrumentation."
)


def generate_review_pairs(functions: list[RustFunction],
                          structs: list[tuple[str, str]]) -> list[dict]:
    """Generate code review SFT pairs — both LGTM and NEEDS_FIXES."""
    pairs = []

    # LGTM pairs from clean public functions
    for fn in functions:
        if fn.is_pub and not fn.is_test and len(fn.body) > 50 and len(fn.body) < 3000:
            code = f"// File: {fn.file_path}\n{fn.doc_comment}\n{fn.signature} {fn.body}" if fn.doc_comment else f"// File: {fn.file_path}\n{fn.signature} {fn.body}"
            pairs.append({
                "messages": [
                    {"role": "system", "content": REVIEWER_SYSTEM},
                    {"role": "user", "content": f"Review this Rust code:\n\n{code[:2000]}"},
                    {"role": "assistant", "content": json.dumps({"issues": [], "verdict": "LGTM"})},
                ],
            })

    # LGTM pairs from clean structs
    for name, defn in structs:
        if len(defn) > 30 and len(defn) < 2000:
            pairs.append({
                "messages": [
                    {"role": "system", "content": REVIEWER_SYSTEM},
                    {"role": "user", "content": f"Review this Rust struct:\n\n{defn}"},
                    {"role": "assistant", "content": json.dumps({"issues": [], "verdict": "LGTM"})},
                ],
            })

    # NEEDS_FIXES pairs — synthetic issues injected into real code
    synthetic_issues = [
        {
            "mutation": lambda c: c.replace("Result<", "/* TODO */ Result<", 1),
            "issue": {"severity": "low", "description": "TODO comment left in production code"},
            "condition": lambda c: "Result<" in c,
        },
        {
            "mutation": lambda c: re.sub(r'(\w+_url):\s*String', r'\1: String, // http://192.168.1.100:8080', c, count=1),
            "issue": {"severity": "high", "description": "Hardcoded IP address in comment — use environment variable or config"},
            "condition": lambda c: "_url" in c and "String" in c,
        },
        {
            "mutation": lambda c: c.replace(".await?", ".await.unwrap()", 1),
            "issue": {"severity": "high", "description": "Using .unwrap() instead of ? operator — will panic on error"},
            "condition": lambda c: ".await?" in c,
        },
    ]

    for fn in functions:
        if fn.is_pub and not fn.is_test and len(fn.body) > 100:
            code = f"{fn.signature} {fn.body}"
            for si in synthetic_issues:
                if si["condition"](code):
                    mutated = si["mutation"](code)
                    if mutated != code:
                        pairs.append({
                            "messages": [
                                {"role": "system", "content": REVIEWER_SYSTEM},
                                {"role": "user", "content": f"Review this Rust code:\n\n{mutated[:2000]}"},
                                {"role": "assistant", "content": json.dumps({
                                    "issues": [si["issue"]],
                                    "verdict": "NEEDS_FIXES",
                                })},
                            ],
                        })
                        break  # one mutation per function

    return pairs


def generate_test_pairs(functions: list[RustFunction]) -> list[dict]:
    """Generate test writing SFT pairs from existing test functions."""
    # Find pairs: non-test function + its corresponding test
    test_fns = {fn.name: fn for fn in functions if fn.is_test}
    non_test_fns = [fn for fn in functions if not fn.is_test and fn.is_pub]

    pairs = []
    for fn in non_test_fns:
        # Look for tests that reference this function
        matching_tests = []
        for tname, tfn in test_fns.items():
            if fn.name in tfn.body:
                matching_tests.append(tfn)

        if matching_tests and len(fn.signature) > 10:
            test_code = "\n\n".join(
                f"#[{'tokio::test' if t.is_async else 'test'}]\n{t.signature} {t.body}"
                for t in matching_tests[:3]  # max 3 tests per function
            )

            context = fn.parent_impl + "\n" if fn.parent_impl else ""
            prompt = (
                f"Generate tests for this function:\n\n"
                f"{context}{fn.doc_comment}\n{fn.signature}"
                f"\n\nFunction body (for reference):\n{fn.body[:1500]}"
            )

            pairs.append({
                "messages": [
                    {"role": "system", "content": TESTER_SYSTEM},
                    {"role": "user", "content": prompt},
                    {"role": "assistant", "content": f"#[cfg(test)]\nmod tests {{\n    use super::*;\n\n{test_code}\n}}"},
                ],
            })

    return pairs


def generate_code_pairs(functions: list[RustFunction],
                        structs: list[tuple[str, str]]) -> list[dict]:
    """Generate code generation SFT pairs — description → implementation."""
    pairs = []

    for fn in functions:
        if fn.is_pub and not fn.is_test and fn.doc_comment and len(fn.body) > 50:
            prompt = (
                f"Implement the following Rust function for Yggdrasil:\n\n"
                f"{fn.doc_comment}\n{fn.signature}"
            )
            pairs.append({
                "messages": [
                    {"role": "system", "content": CODER_SYSTEM},
                    {"role": "user", "content": prompt},
                    {"role": "assistant", "content": f"{fn.signature} {fn.body}"},
                ],
            })

    for name, defn in structs:
        if "Serialize" in defn and "Deserialize" in defn:
            # Extract field names for the prompt
            fields = re.findall(r'pub\s+(\w+):\s*([^,\n]+)', defn)
            if fields:
                field_desc = ", ".join(f"{n}: {t.strip()}" for n, t in fields[:8])
                prompt = (
                    f"Generate a Yggdrasil config struct named {name} with fields: {field_desc}. "
                    f"Include serde derives and default functions where appropriate."
                )
                pairs.append({
                    "messages": [
                        {"role": "system", "content": CODER_SYSTEM},
                        {"role": "user", "content": prompt},
                        {"role": "assistant", "content": defn},
                    ],
                })

    return pairs


# ─────────────────────────────────────────────────────────────────
# Main
# ─────────────────────────────────────────────────────────────────

def walk_crates(crate_dir: Path) -> tuple[list[RustFunction], list[tuple[str, str]]]:
    """Walk all .rs files in crate directory, extract functions and structs."""
    all_fns = []
    all_structs = []

    for rs_file in sorted(crate_dir.rglob("*.rs")):
        # Skip target dir, test fixtures
        if "target" in str(rs_file) or "fixtures" in str(rs_file):
            continue

        try:
            source = rs_file.read_text()
        except (OSError, UnicodeDecodeError):
            continue

        rel_path = str(rs_file.relative_to(crate_dir))
        fns = extract_functions(source, rel_path)
        structs = extract_structs(source)

        all_fns.extend(fns)
        all_structs.extend(structs)

    return all_fns, all_structs


def write_jsonl(data: list[dict], path: Path):
    """Write list of dicts as JSONL."""
    with open(path, "w") as f:
        for entry in data:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")


def main():
    parser = argparse.ArgumentParser(description="Prepare SFT data for Yggdrasil code specialists")
    parser.add_argument("--crate-dir", type=Path, default=CRATE_DIR, help="Path to crates/ directory")
    parser.add_argument("--output-dir", type=Path, default=Path(__file__).parent / "data")
    parser.add_argument("--augment", action="store_true", help="Generate augmented synthetic examples")
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)

    print(f"Scanning crates at: {args.crate_dir}")
    functions, structs = walk_crates(args.crate_dir)
    print(f"  Found {len(functions)} functions, {len(structs)} structs")

    pub_fns = [f for f in functions if f.is_pub and not f.is_test]
    test_fns = [f for f in functions if f.is_test]
    print(f"  Public functions: {pub_fns.__len__()}, Tests: {test_fns.__len__()}")

    # Generate datasets
    print("\nGenerating review pairs...")
    review_pairs = generate_review_pairs(functions, structs)
    write_jsonl(review_pairs, args.output_dir / "review_train.jsonl")
    print(f"  → {len(review_pairs)} review pairs")

    print("Generating test pairs...")
    test_pairs = generate_test_pairs(functions)
    write_jsonl(test_pairs, args.output_dir / "test_train.jsonl")
    print(f"  → {len(test_pairs)} test pairs")

    print("Generating code gen pairs...")
    code_pairs = generate_code_pairs(functions, structs)
    write_jsonl(code_pairs, args.output_dir / "code_train.jsonl")
    print(f"  → {len(code_pairs)} code gen pairs")

    # Summary
    total = len(review_pairs) + len(test_pairs) + len(code_pairs)
    print(f"\n{'=' * 50}")
    print(f"Total SFT examples: {total}")
    print(f"  review_train.jsonl:  {len(review_pairs)}")
    print(f"  test_train.jsonl:    {len(test_pairs)}")
    print(f"  code_train.jsonl:    {len(code_pairs)}")
    print(f"Output: {args.output_dir}")
    print(f"{'=' * 50}")

    if total < 100:
        print("\nWARNING: Dataset is small. Consider running with --augment "
              "or using training/saga/augment_with_llm.py to generate more examples.")


if __name__ == "__main__":
    main()
